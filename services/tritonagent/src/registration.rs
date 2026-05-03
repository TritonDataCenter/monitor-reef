// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Boot-time self-registration / resume flow for tritonagent.
//!
//! On startup the agent has one of two states:
//!
//! 1. **Already paired.** A credential file exists on disk; we just
//!    read it and proceed.
//! 2. **Fresh CN.** No credential file. The agent calls
//!    `POST /v2/agent/register` with its sysinfo, prints the returned
//!    claim code on the console (and into a scrape-friendly file), then
//!    long-polls `GET /v2/agent/register/status` until an operator (or
//!    the auto-approve window) yields the per-CN API key. The key is
//!    persisted via [`crate::credentials::save`] before the function
//!    returns, so a crash during the very next operation does not lose
//!    it.
//!
//! Tritond's `register/status` endpoint long-polls server-side for ~30s
//! per call; the agent's job here is to keep re-issuing the long-poll
//! until either approval lands, the registration is disabled by an
//! operator, or the caller-supplied `register_timeout` deadline is hit.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{info, warn};
use tritond_client::Client;
use tritond_client::types::{CnState, RegisterCnRequest};
use tritond_cn_platform::smartos::Sysinfo;
use uuid::Uuid;

use crate::credentials;

/// Where the active claim code is mirrored to disk so ops scripts
/// (`tcadm cn approve --code "$(cat …)"`) can scrape it without
/// scraping the agent's stderr.
///
/// Best-effort: the registration flow logs a warning if it cannot
/// write this file, but does *not* fail. The console log is
/// canonical; this file is just a convenience.
pub const CLAIM_CODE_PATH: &str = "/var/lib/tritonagent/claim-code";

/// Sleep between consecutive long-polls when tritond returns
/// `state = Pending` without `api_key`. Each individual long-poll is
/// already ~30s server-side, so we add a small client-side gap to
/// keep a reconnect storm from looking like a DoS during a control
/// plane bounce.
const PENDING_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Sleep between retries when a single long-poll fails with a
/// (potentially) transient error — connection reset, 5xx, etc. The
/// server endpoint is anonymous and idempotent, so retrying is safe.
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Resolve the agent's per-CN API key, registering with tritond if
/// no credential is on disk yet.
///
/// Returns the wire-form `tcadm_…` plaintext on success. The caller
/// is expected to feed it to [`crate::AgentConfig::api_key`].
///
/// Behavior:
///
/// 1. If [`credentials::load`] returns `Some`, return that — no
///    network traffic, no logging beyond a debug breadcrumb.
/// 2. Otherwise, build an anonymous tritond client, `POST /register`
///    with the sysinfo, and surface the claim code (console + scrape
///    file).
/// 3. Long-poll `/register/status` with the returned `poll_token`.
///    Each iteration sleeps the server for up to ~30s. Repeat until
///    state flips to `Approved` *and* `api_key` is `Some` — that is
///    the one shot the agent gets at the credential. If state flips
///    to `Disabled`, return an error. If `register_timeout` elapses,
///    return a timeout error so the supervisor can decide what to
///    do.
/// 4. Persist the credential via [`credentials::save`] before
///    returning. Best-effort delete the claim-code scrape file once
///    we no longer need it.
pub async fn register_or_resume(
    endpoint: &str,
    sysinfo: &Sysinfo,
    server_uuid: Uuid,
    credential_path: &Path,
    register_timeout: Duration,
) -> Result<String> {
    if let Some(existing) = credentials::load(credential_path)
        .with_context(|| format!("load credential at {}", credential_path.display()))?
    {
        info!(
            credential_path = %credential_path.display(),
            "resumed from persisted credential",
        );
        return Ok(existing);
    }

    let client = build_anonymous_client(endpoint).context("build anonymous tritond client")?;

    let hostname = sysinfo
        .hostname()
        .ok_or_else(|| anyhow::anyhow!("sysinfo lacks Hostname; cannot register"))?
        .to_string();
    let admin_ip = sysinfo.admin_ip();

    let register_req = RegisterCnRequest {
        admin_ip,
        hostname: hostname.clone(),
        server_uuid,
        sysinfo: sysinfo.raw.clone(),
    };

    let response = client
        .agent_register()
        .body(register_req)
        .send()
        .await
        .with_context(|| {
            format!("POST {endpoint}/v2/agent/register (server_uuid={server_uuid})")
        })?
        .into_inner();

    let poll_token = response.poll_token;

    // The auto-approve fast path returns Approved immediately; the
    // first long-poll will then deliver the api_key. The Pending path
    // returns a claim_code we must surface to the operator.
    if let Some(code) = response.claim_code.as_deref() {
        announce_claim_code(code);
    } else if response.state == CnState::Approved {
        info!(
            state = %response.state,
            "tritond accepted registration in the auto-approve window; awaiting credential",
        );
    } else {
        // Pending without a claim_code is a tritond-side bug or a
        // protocol mismatch; surface clearly.
        warn!(
            state = %response.state,
            "registration response had no claim_code and is not Approved; long-poll may stall",
        );
    }

    let key = await_credential(&client, &poll_token, register_timeout).await?;

    credentials::save(credential_path, &key)
        .with_context(|| format!("persist credential to {}", credential_path.display()))?;
    info!(
        credential_path = %credential_path.display(),
        "approved; persisted credential",
    );

    // Drop the scrape file now that the operator no longer needs it.
    // Best-effort: it might not exist (auto-approve) and it might be
    // on a read-only mount in tests. Either way we do not fail.
    let _ = std::fs::remove_file(CLAIM_CODE_PATH);

    Ok(key)
}

/// Construct an anonymous tritond client with the bundled webpki
/// roots, mirroring the TLS posture of [`crate::AgentConfig::build_client`]
/// but without an `Authorization` header (this is a pre-credential
/// call).
fn build_anonymous_client(endpoint: &str) -> Result<Client> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let http = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .build()
        .context("build anonymous reqwest client")?;
    Ok(Client::new_with_client(endpoint, http))
}

/// Print the claim code to stderr and mirror it to the scrape file.
///
/// One conspicuous line on stderr (everything between the leading
/// `===` markers) so it survives being interleaved with tracing
/// output. The line includes the literal `tcadm cn approve` command
/// the operator should run, with the code already substituted.
fn announce_claim_code(code: &str) {
    // Eprintln deliberately bypasses the tracing layer here so the
    // message format is stable for ops runbooks even if the operator
    // changes the tracing filter or formatter.
    eprintln!("===");
    eprintln!(
        "REGISTRATION CLAIM CODE: {code} \
         (operator: tcadm cn approve --code {code})"
    );
    eprintln!("===");
    info!(claim_code = %code, "awaiting approval, claim code displayed on console");

    // Best-effort scrape file. Failure here is non-fatal; we already
    // printed to stderr.
    if let Some(parent) = Path::new(CLAIM_CODE_PATH).parent()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(
            error = %e,
            parent = %parent.display(),
            "could not create claim-code scrape directory; \
             console log is still authoritative",
        );
        return;
    }
    if let Err(e) = std::fs::write(CLAIM_CODE_PATH, format!("{code}\n")) {
        warn!(
            error = %e,
            path = CLAIM_CODE_PATH,
            "could not write claim-code scrape file; \
             console log is still authoritative",
        );
    }
}

/// Long-poll tritond's `/v2/agent/register/status` until the
/// per-CN API key arrives, the registration is disabled, or
/// `register_timeout` elapses.
async fn await_credential(
    client: &Client,
    poll_token: &str,
    register_timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + register_timeout;
    let mut last_logged_state: Option<CnState> = None;

    loop {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "registration was not approved within {} seconds; \
                 operator must run `tcadm cn approve --code …`",
                register_timeout.as_secs(),
            );
        }

        let send_result = client
            .agent_register_status()
            .poll_token(poll_token.to_string())
            .send()
            .await;

        let response = match send_result {
            Ok(r) => r.into_inner(),
            Err(e) => {
                // 404 from a long-poll means the poll_token is not
                // recognised — usually because the registration was
                // explicitly removed by an operator. There is no
                // recovery path from inside this function.
                if e.status() == Some(reqwest::StatusCode::NOT_FOUND) {
                    return Err(anyhow::anyhow!(
                        "tritond returned 404 for register/status: \
                         poll token unknown (registration removed?)"
                    ))
                    .context("agent_register_status");
                }
                if e.is_retryable() {
                    warn!(
                        error = %e,
                        "transient error long-polling register/status; backing off",
                    );
                    tokio::time::sleep(TRANSIENT_RETRY_DELAY).await;
                    continue;
                }
                return Err(anyhow::Error::msg(e.to_string()))
                    .context("agent_register_status (non-retryable)");
            }
        };

        if Some(response.state) != last_logged_state {
            info!(state = %response.state, "register/status update");
            last_logged_state = Some(response.state);
        }

        match response.state {
            CnState::Approved => {
                if let Some(key) = response.api_key {
                    return Ok(key);
                }
                // Approved but no api_key means tritond already handed
                // it out on a previous call — the credential file must
                // have been deleted out from under us. The only way
                // out is operator intervention (`tcadm cn disable`
                // then re-register).
                anyhow::bail!(
                    "registration is Approved but tritond returned no api_key; \
                     a previous agent process consumed the one-shot credential. \
                     Operator must run `tcadm cn disable` and let the agent re-register."
                );
            }
            CnState::Disabled => {
                anyhow::bail!(
                    "CN registration was disabled by an operator before approval"
                );
            }
            CnState::Pending => {
                tokio::time::sleep(PENDING_RETRY_DELAY).await;
                continue;
            }
        }
    }
}
