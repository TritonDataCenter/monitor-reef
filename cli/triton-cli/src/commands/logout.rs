// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `triton logout` — revoke the access token and delete local state.
//!
//! Logout is idempotent: if there is no token file, we report "Not
//! logged in" and exit 0. If there is a token file, we attempt a
//! best-effort `POST /v1/auth/logout` and always delete the local file
//! regardless of whether the server call succeeded — the operator's
//! intent is "forget this credential on my machine", which is always
//! achievable locally.

use std::sync::Arc;

use anyhow::{Result, anyhow};

use triton_gateway_client::TypedClient as GatewayClient;
use triton_gateway_client::auth::GatewayAuthConfig;

use crate::auth::{FileTokenProvider, StoredTokens};
use crate::config::resolve_profile;

pub async fn run(profile_override: Option<&str>) -> Result<()> {
    let profile = resolve_profile(profile_override).await?;
    let api = profile.require_triton_api().map_err(|e| {
        anyhow!("{e}\n\nHint: `triton logout` only operates on tritonapi profiles.")
    })?;

    let Some(_tokens) = StoredTokens::load(profile.name()).await? else {
        eprintln!(
            "Not logged in (no token file for profile '{}')",
            profile.name()
        );
        return Ok(());
    };

    // Best-effort server-side revoke. We wire up a real TokenProvider so
    // the gateway-client stamps the current access token; if the
    // provider's proactive-refresh fires and fails, we still carry on
    // with deletion (the whole point of logout is to forget the
    // credential regardless).
    let provider: Arc<FileTokenProvider> =
        FileTokenProvider::load(profile.name(), &api.url, api.insecure).await?;
    let gw_cfg = GatewayAuthConfig::bearer(provider);
    let http_client = triton_tls::build_http_client(api.insecure)
        .await
        .map_err(|e| anyhow!("building HTTP client: {e}"))?;
    let client = GatewayClient::new_with_http_client(&api.url, gw_cfg, http_client);

    if let Err(e) = client.inner().auth_logout().send().await {
        eprintln!("triton: warning: server-side logout failed (proceeding with local delete): {e}");
    }

    StoredTokens::delete(profile.name()).await?;
    eprintln!("Logged out");
    Ok(())
}
