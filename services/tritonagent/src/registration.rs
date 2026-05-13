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
use tritond_auth::CONSOLE_TICKET_KEY_BYTES;
use tritond_client::Client;
use tritond_client::types::{CnState, RegisterCnRequest};
use tritond_cn_platform::smartos::Sysinfo;
use uuid::Uuid;

use crate::{console_creds, credentials};

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

/// What [`register_or_resume`] hands back: the per-CN API key (always)
/// plus the per-CN console-ticket key and IMDS token key (each when
/// known -- see below).
pub struct RegistrationOutcome {
    /// Wire-form `tcadm_…` API key for every `/v2/agent/*` call.
    pub api_key: String,
    /// Per-CN HS256 console-ticket key. `None` when the agent resumed
    /// from a credential file written before this feature shipped (or
    /// the console-credentials file was lost). The console listener is
    /// not started in that case; the operator must `tcadm cn disable`
    /// and let the agent re-register to get one.
    pub console_ticket_key: Option<[u8; CONSOLE_TICKET_KEY_BYTES]>,
    /// Per-CN HS256 IMDSv2 session-token key. Same delivery contract
    /// as `console_ticket_key`; `None` when the agent resumed from a
    /// credential file written before IMDS shipped (or the
    /// imds-credentials file was lost). The IMDS listener is not
    /// started in that case. See `IMDS_DESIGN.md` §3.
    pub imds_token_key: Option<[u8; tritond_auth::IMDS_TOKEN_KEY_BYTES]>,
}

/// Resolve the agent's per-CN API key + console-ticket key, registering
/// with tritond if no credential is on disk yet.
///
/// The caller passes the console listener's port + the lowercase-hex
/// SHA-256 of its TLS leaf-cert SPKI; these are sent on every
/// (re-)registration so tritond knows where to dial and what cert to
/// pin.
///
/// Behavior:
///
/// 1. If [`credentials::load`] returns `Some`, return that key plus
///    whatever console-ticket key [`console_creds::load_console_ticket_key`]
///    finds on disk (possibly `None` — see [`RegistrationOutcome`]).
///    No network traffic.
/// 2. Otherwise, build an anonymous tritond client, `POST /register`
///    with the sysinfo + console fields, and surface the claim code
///    (console + scrape file).
/// 3. Long-poll `/register/status` with the returned `poll_token`.
///    Repeat until state flips to `Approved` *and* `api_key` is `Some`
///    — that is the one shot the agent gets at both the credential and
///    the console-ticket key. If state flips to `Disabled`, return an
///    error. If `register_timeout` elapses, return a timeout error.
/// 4. Persist the credential via [`credentials::save`] and the
///    console-ticket key via [`console_creds::save_console_ticket_key`]
///    before returning. Best-effort delete the claim-code scrape file.
pub async fn register_or_resume(
    endpoint: &str,
    sysinfo: &Sysinfo,
    server_uuid: Uuid,
    credential_path: &Path,
    console_listen_port: u16,
    console_tls_spki_sha256_hex: String,
    register_timeout: Duration,
) -> Result<RegistrationOutcome> {
    if let Some(existing) = credentials::load(credential_path)
        .with_context(|| format!("load credential at {}", credential_path.display()))?
    {
        info!(
            credential_path = %credential_path.display(),
            "resumed from persisted credential",
        );
        let console_ticket_key = console_creds::load_console_ticket_key(credential_path)
            .with_context(|| {
                format!(
                    "load console-ticket key alongside {}",
                    credential_path.display()
                )
            })?;
        let imds_token_key =
            crate::imds_creds::load_imds_token_key(credential_path).with_context(|| {
                format!(
                    "load IMDS token key alongside {}",
                    credential_path.display()
                )
            })?;
        if console_ticket_key.is_none() {
            warn!(
                "no per-CN console-ticket key on disk (agent registered before consoles \
                 were supported, or the console-credentials file was lost); serial / VNC \
                 consoles are unavailable for this CN until it re-registers \
                 (`tcadm cn disable` then approve again)",
            );
        }
        return Ok(RegistrationOutcome {
            api_key: existing,
            console_ticket_key,
            imds_token_key,
        });
    }

    let client = build_anonymous_client(endpoint).context("build anonymous tritond client")?;

    let hostname = sysinfo
        .hostname()
        .ok_or_else(|| anyhow::anyhow!("sysinfo lacks Hostname; cannot register"))?
        .to_string();
    let admin_ip = sysinfo.admin_ip();

    let register_req = RegisterCnRequest {
        admin_ip,
        console_listen_port: Some(console_listen_port),
        console_tls_spki_sha256_hex: Some(console_tls_spki_sha256_hex),
        hostname: hostname.clone(),
        server_uuid,
        sysinfo: sysinfo.raw.clone(),
    };

    let response = client
        .agent_register()
        .body(register_req)
        .send()
        .await
        .with_context(|| format!("POST {endpoint}/v2/agent/register (server_uuid={server_uuid})"))?
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

    let (key, console_ticket_key_hex, imds_token_key_hex) =
        await_credential(&client, &poll_token, register_timeout).await?;

    credentials::save(credential_path, &key)
        .with_context(|| format!("persist credential to {}", credential_path.display()))?;
    info!(
        credential_path = %credential_path.display(),
        "approved; persisted credential",
    );

    // Decode + persist the per-CN console-ticket key. tritond is
    // expected to always send it alongside a freshly-minted api_key; if
    // it didn't (e.g. an older tritond), warn and carry on without the
    // console listener rather than failing the whole agent.
    let console_ticket_key = match console_ticket_key_hex {
        Some(hex_str) => match decode_console_ticket_key(&hex_str) {
            Ok(bytes) => {
                console_creds::save_console_ticket_key(credential_path, &bytes).with_context(
                    || {
                        format!(
                            "persist console-ticket key alongside {}",
                            credential_path.display()
                        )
                    },
                )?;
                info!("persisted per-CN console-ticket key");
                Some(bytes)
            }
            Err(e) => {
                warn!(error = %e, "tritond returned a malformed console-ticket key; console disabled");
                None
            }
        },
        None => {
            warn!(
                "registration response carried no console-ticket key; serial / VNC consoles \
                 are unavailable for this CN",
            );
            None
        }
    };

    // Decode + persist the per-CN IMDS token key. Same delivery
    // contract + same fallback as the console-ticket key above.
    let imds_token_key = match imds_token_key_hex {
        Some(hex_str) => match decode_imds_token_key(&hex_str) {
            Ok(bytes) => {
                crate::imds_creds::save_imds_token_key(credential_path, &bytes).with_context(
                    || {
                        format!(
                            "persist IMDS token key alongside {}",
                            credential_path.display()
                        )
                    },
                )?;
                info!("persisted per-CN IMDS token key");
                Some(bytes)
            }
            Err(e) => {
                warn!(error = %e, "tritond returned a malformed IMDS token key; IMDS disabled");
                None
            }
        },
        None => {
            warn!(
                "registration response carried no IMDS token key; IMDS is unavailable for this CN",
            );
            None
        }
    };

    // Drop the scrape file now that the operator no longer needs it.
    // Best-effort: it might not exist (auto-approve) and it might be
    // on a read-only mount in tests. Either way we do not fail.
    let _ = std::fs::remove_file(CLAIM_CODE_PATH);

    Ok(RegistrationOutcome {
        api_key: key,
        console_ticket_key,
        imds_token_key,
    })
}

/// Decode a lowercase-hex (64-char) IMDS token key into 32 bytes.
fn decode_imds_token_key(hex_str: &str) -> Result<[u8; tritond_auth::IMDS_TOKEN_KEY_BYTES]> {
    let bytes = hex::decode(hex_str.trim()).context("IMDS token key is not valid lowercase hex")?;
    if bytes.len() != tritond_auth::IMDS_TOKEN_KEY_BYTES {
        anyhow::bail!(
            "IMDS token key is {} bytes, expected {}",
            bytes.len(),
            tritond_auth::IMDS_TOKEN_KEY_BYTES,
        );
    }
    let mut out = [0u8; tritond_auth::IMDS_TOKEN_KEY_BYTES];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Decode a lowercase-hex (64-char) console-ticket key into 32 bytes.
fn decode_console_ticket_key(hex_str: &str) -> Result<[u8; CONSOLE_TICKET_KEY_BYTES]> {
    let bytes =
        hex::decode(hex_str.trim()).context("console-ticket key is not valid lowercase hex")?;
    if bytes.len() != CONSOLE_TICKET_KEY_BYTES {
        anyhow::bail!(
            "console-ticket key is {} bytes, expected {}",
            bytes.len(),
            CONSOLE_TICKET_KEY_BYTES,
        );
    }
    let mut out = [0u8; CONSOLE_TICKET_KEY_BYTES];
    out.copy_from_slice(&bytes);
    Ok(out)
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
///
/// Returns `(api_key, console_ticket_key_hex, imds_token_key_hex)`.
/// Each post-key field is `Some` when tritond is current and that
/// per-CN feature has been wired; an older tritond may omit them,
/// which the caller treats as "that listener unavailable" rather
/// than fatal.
async fn await_credential(
    client: &Client,
    poll_token: &str,
    register_timeout: Duration,
) -> Result<(String, Option<String>, Option<String>)> {
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
                    return Ok((
                        key,
                        response.console_ticket_key_hex,
                        response.imds_token_key_hex,
                    ));
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
                anyhow::bail!("CN registration was disabled by an operator before approval");
            }
            CnState::Pending => {
                tokio::time::sleep(PENDING_RETRY_DELAY).await;
                continue;
            }
        }
    }
}
