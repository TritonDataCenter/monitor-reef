// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `triton login` — obtain a Bearer JWT from a triton-gateway.
//!
//! Requires the resolved profile to be tritonapi-kind. The profile must
//! already exist on disk (MVP decision, per the Phase 3 plan): `login`
//! will not create a new profile from ad-hoc `--url`/`--account` flags.
//! A separate `triton profile create --auth tritonapi ...` subcommand is
//! the right place for that and can be added as a follow-up without
//! changing any of the login flow.
//!
//! Stdout is intentionally kept quiet (reserved for future scriptable
//! output, e.g. `--json` on the session info). All user-facing text goes
//! to stderr.

use anyhow::{Context as _, Result, anyhow};
use clap::Args;

use triton_gateway_client::TypedClient as GatewayClient;
use triton_gateway_client::auth::{GatewayAuthConfig, GatewayAuthMethod, TokenProvider};
use triton_gateway_client::types::LoginRequest;

use crate::auth::{StoredTokens, jwt};
use crate::config::resolve_profile;

/// `triton login` arguments.
#[derive(Args, Clone, Debug)]
pub struct LoginArgs {
    /// Username (defaults to the profile's `account`).
    #[arg(short, long)]
    pub username: Option<String>,
}

pub async fn run(args: LoginArgs, profile_override: Option<&str>) -> Result<()> {
    let profile = resolve_profile(profile_override).await?;
    let api = profile.require_triton_api().map_err(|e| {
        // Swap the generic trait-object error for a login-specific one
        // that suggests the fix.
        anyhow!(
            "{e}\n\nHint: `triton login` only works with tritonapi (JWT) profiles. \
             Create one with `triton profile create` and set `auth` to `tritonapi` \
             in the profile JSON."
        )
    })?;

    // Default username is the profile's account; allow CLI override and
    // let the operator confirm interactively.
    let default_username = args.username.unwrap_or_else(|| api.account.clone());
    let username = prompt_username(&default_username)?;
    let password = read_password(&username)?;
    if password.is_empty() {
        return Err(anyhow!("password is empty; aborting without a token write"));
    }

    // Build a no-auth gateway client just for the login call — login
    // itself is unauthenticated (the whole point is to *obtain* a
    // token). We implement this by supplying a TokenProvider that is
    // never consulted: `/v1/auth/login` does not go through
    // `add_auth_headers` with a real Bearer token.
    //
    // In practice the gateway-client's pre_hook ignores 401 handling
    // here; we call the login builder directly which still runs the
    // hook, so we need a trivial TokenProvider to satisfy the type.
    let bootstrap_provider =
        std::sync::Arc::new(PreLoginProvider) as std::sync::Arc<dyn TokenProvider>;
    let gw_cfg = GatewayAuthConfig {
        method: GatewayAuthMethod::Bearer(bootstrap_provider),
        accept_version: None,
        act_as: None,
    };

    let http_client = triton_tls::build_http_client(api.insecure)
        .await
        .map_err(|e| anyhow!("building HTTP client: {e}"))?;
    let client = GatewayClient::new_with_http_client(&api.url, gw_cfg, http_client);

    let response = client
        .inner()
        .auth_login()
        .body(LoginRequest {
            username: username.clone(),
            password,
        })
        .send()
        .await
        .map_err(|e| translate_login_error(e, &api.url))?
        .into_inner();

    let expires_at = jwt::extract_exp(&response.token)
        .context("gateway returned a token without a readable `exp` claim")?;

    let tokens = StoredTokens {
        access_token: response.token,
        refresh_token: response.refresh_token,
        expires_at,
        gateway_url: api.url.clone(),
    };
    tokens.save(profile.name()).await?;

    // Successful login: tell the operator what happened (to stderr so
    // stdout stays scriptable). No token values are printed.
    eprintln!(
        "Logged in as {} ({}). Token expires {}.",
        response.user.username,
        response.user.id,
        expires_at.to_rfc3339()
    );
    Ok(())
}

/// Read the password interactively from the terminal, with an env-var
/// escape hatch for non-interactive contexts (CI / integration tests).
///
/// `TRITON_PASSWORD` is unset on the returned process so it does not
/// leak into child processes or logs. It is intentionally undocumented
/// in `--help` — its use is to enable the integration test harness and
/// automation workflows; operators should use the interactive prompt.
fn read_password(username: &str) -> Result<String> {
    if let Ok(pw) = std::env::var("TRITON_PASSWORD") {
        // Best-effort: remove the var so later code (e.g. an exec'd
        // child) doesn't inherit it.
        // SAFETY: single-threaded startup code; no other thread races this.
        unsafe {
            std::env::remove_var("TRITON_PASSWORD");
        }
        return Ok(pw);
    }
    // Fall back to prompting. `rpassword` reads from the controlling
    // terminal, not stdin — if there is no tty (e.g., piped input), it
    // returns an error which we surface as-is.
    let _ = username; // kept for future prompt personalisation
    rpassword::prompt_password("Password: ").context("reading password from terminal")
}

/// Prompt for the username, offering `default` as-is.
fn prompt_username(default: &str) -> Result<String> {
    eprint!("Username [{default}]: ");
    use std::io::Write as _;
    std::io::stderr().flush().ok();

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading username from stdin")?;
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed)
    }
}

fn translate_login_error(
    err: triton_gateway_client::Error<triton_gateway_client::types::Error>,
    gateway_url: &str,
) -> anyhow::Error {
    use triton_gateway_client::Error as GwErr;
    match err {
        GwErr::ErrorResponse(resp) => {
            let body = resp.into_inner();
            let code = body.error_code.as_deref().unwrap_or("(no error_code)");
            anyhow!("login rejected by {gateway_url}: [{code}] {}", body.message)
        }
        GwErr::CommunicationError(e) => {
            anyhow!("failed to reach gateway {gateway_url}: {e}")
        }
        other => anyhow!("login request failed: {other}"),
    }
}

/// Dummy TokenProvider used to satisfy the gateway-client's
/// `pre_hook_async` type during the `/v1/auth/login` call.
///
/// The login endpoint does not require a bearer token; the pre-hook
/// will still run but an empty bearer is acceptable for the first call.
/// The login endpoint does not inspect the Authorization header (see
/// the triton-gateway auth code) — it authenticates via username+password
/// in the JSON body.
struct PreLoginProvider;

#[async_trait::async_trait]
impl TokenProvider for PreLoginProvider {
    async fn current_token(&self) -> anyhow::Result<String> {
        // Empty string produces `Authorization: Bearer ` which the
        // gateway ignores for /v1/auth/login. It does not appear in
        // logs.
        Ok(String::new())
    }

    async fn on_unauthorized(&self) -> anyhow::Result<()> {
        // Login does not retry — a 401 from the login endpoint means
        // bad credentials, not an expired session.
        Err(anyhow!("login endpoint returned 401"))
    }
}
