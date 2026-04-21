// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `triton whoami` — show the identity backing the current token.
//!
//! Calls `GET /v1/auth/session` through the gateway client, which the
//! gateway answers from the JWT claims (no LDAP round-trip). Uses the
//! [`FileTokenProvider`] so a proactive refresh runs if the token is
//! close to expiry.
//!
//! Output goes to stdout in a plain key/value format suitable for
//! scripting; with `--json` the raw response body is pretty-printed.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use clap::Args;

use triton_gateway_client::TypedClient as GatewayClient;
use triton_gateway_client::auth::GatewayAuthConfig;

use crate::auth::{FileTokenProvider, StoredTokens};
use crate::config::resolve_profile;
use crate::output::json;

/// `triton whoami` flags.
///
/// The global `-j/--json` flag on the top-level CLI is wired in via
/// `Cli::json`; this args struct is kept for future whoami-specific
/// flags (e.g. a `--refresh` to force a proactive refresh before
/// calling `/v1/auth/session`).
#[derive(Args, Clone, Debug, Default)]
pub struct WhoamiArgs {}

pub async fn run(args: WhoamiArgs, profile_override: Option<&str>, use_json: bool) -> Result<()> {
    let _ = args; // reserved for future flags
    let json_out = use_json;
    let profile = resolve_profile(profile_override).await?;
    let api = profile.require_triton_api().map_err(|e| {
        anyhow!("{e}\n\nHint: `triton whoami` only operates on tritonapi profiles.")
    })?;

    // Make sure there's a token to read — produces the "run triton
    // login" message right away rather than after a 401 round-trip.
    let tokens = StoredTokens::load(profile.name()).await?.ok_or_else(|| {
        anyhow!(
            "Not logged in — run `triton login -p {}` to authenticate.",
            profile.name()
        )
    })?;

    let provider: Arc<FileTokenProvider> = Arc::new(FileTokenProvider::new(
        profile.name(),
        tokens,
        &api.url,
        api.insecure,
    ));
    let gw_cfg = GatewayAuthConfig::bearer(provider);
    let http_client = triton_tls::build_http_client(api.insecure)
        .await
        .map_err(|e| anyhow!("building HTTP client: {e}"))?;
    let client = GatewayClient::new_with_http_client(&api.url, gw_cfg, http_client);

    let response = client.inner().auth_session().send().await.map_err(|e| {
        use triton_gateway_client::Error as GwErr;
        match e {
            GwErr::ErrorResponse(r) => {
                let body = r.into_inner();
                let code = body.error_code.as_deref().unwrap_or("(no error_code)");
                if body.message.to_lowercase().contains("token")
                    || code.to_lowercase().contains("unauth")
                {
                    anyhow!(
                        "session check rejected: [{code}] {}. \
                         Run `triton login -p {}` to re-authenticate.",
                        body.message,
                        profile.name()
                    )
                } else {
                    anyhow!("session check failed: [{code}] {}", body.message)
                }
            }
            other => anyhow!("session request failed: {other}"),
        }
    })?;

    let session = response.into_inner();
    let user = session.user;

    if json_out {
        json::print_json(&serde_json::json!({
            "id": user.id,
            "username": user.username,
            "email": user.email,
            "name": user.name,
            "company": user.company,
            "is_admin": user.is_admin,
        }))?;
    } else {
        // Look up expires_at locally — /v1/auth/session doesn't echo
        // it back, but we stored it at login.
        let local = StoredTokens::load(profile.name()).await?;
        println!("username: {}", user.username);
        println!("id:       {}", user.id);
        println!("is_admin: {}", user.is_admin);
        if let Some(email) = &user.email {
            println!("email:    {email}");
        }
        if let Some(name) = &user.name {
            println!("name:     {name}");
        }
        if let Some(company) = &user.company {
            println!("company:  {company}");
        }
        if let Some(t) = &local {
            println!("expires:  {}", t.expires_at.to_rfc3339());
        }
    }

    Ok(())
}
