// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! File-backed [`TokenProvider`] implementation used by login / logout /
//! whoami (and, in Phase 4, by every cloudapi-style command run under a
//! tritonapi profile).
//!
//! ```text
//!                        ┌───────────────────┐
//!                        │  FileTokenProvider │
//!                        ├───────────────────┤
//!                 ┌─────▶│ current_token()   │───▶ on-disk mutex ──▶ read
//!                 │      │   (proactive refresh if <30s to exp)    │
//! gateway client  │      │ on_unauthorized() │───▶ POST /v1/auth/refresh
//!                 │      └───────────────────┘
//!                 │
//!                 └──── Arc<dyn TokenProvider> ────┘
//! ```
//!
//! The provider owns the tokens under a `tokio::sync::Mutex` so two
//! concurrent requests on the same client can't both trigger a refresh
//! and invalidate each other's freshly-issued refresh token.
#![allow(dead_code)] // public API consumed by login/logout/whoami commits

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::Mutex;

use triton_gateway_client::TokenProvider;
use triton_gateway_client::types::RefreshRequest;

use super::jwt;
use super::tokens::StoredTokens;

/// Refresh access tokens this many seconds before they expire.
///
/// 30s leaves enough slack that a request landing right at the threshold
/// can complete without the gateway rejecting mid-flight, while still
/// avoiding gratuitous refreshes for the common case (login → command
/// within a few minutes).
const PROACTIVE_REFRESH_WINDOW_SECS: i64 = 30;

/// HTTP timeout for the refresh call itself. Refresh is synchronous in
/// the user's command flow so we want to fail fast on a dead gateway
/// rather than pinning the CLI for minutes.
const REFRESH_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// File-backed implementation of
/// [`triton_gateway_client::TokenProvider`].
pub struct FileTokenProvider {
    profile: String,
    gateway_url: String,
    insecure: bool,
    /// Mutated on every refresh (proactive or reactive). Held through
    /// the refresh network call so concurrent callers on this provider
    /// instance serialize rather than racing.
    state: Mutex<StoredTokens>,
}

impl FileTokenProvider {
    /// Construct a provider from an already-loaded [`StoredTokens`].
    ///
    /// The provider remembers `gateway_url` and `insecure` so the
    /// reactive-refresh call doesn't depend on external state.
    pub fn new(
        profile: impl Into<String>,
        tokens: StoredTokens,
        gateway_url: impl Into<String>,
        insecure: bool,
    ) -> Self {
        Self {
            profile: profile.into(),
            gateway_url: gateway_url.into(),
            insecure,
            state: Mutex::new(tokens),
        }
    }

    /// Load the profile's tokens from disk and wrap them in a provider.
    ///
    /// Returns a user-facing error if no token file exists — the caller
    /// should prompt the operator to run `triton login`.
    pub async fn load(
        profile: &str,
        gateway_url: &str,
        insecure: bool,
    ) -> anyhow::Result<Arc<Self>> {
        let tokens = StoredTokens::load(profile).await?.ok_or_else(|| {
            anyhow!(
                "No stored tokens for profile '{}'. Run `triton login` first.",
                profile
            )
        })?;
        Ok(Arc::new(Self::new(profile, tokens, gateway_url, insecure)))
    }

    /// Name of the profile these tokens belong to.
    #[allow(dead_code)]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Attempt a refresh using the stored refresh token, persist the
    /// new pair, and mutate `state` in place. Returns `Err` if the
    /// gateway rejects the refresh token — that is an unrecoverable
    /// failure and the caller should ask the operator to re-login.
    async fn do_refresh(&self, state: &mut StoredTokens) -> anyhow::Result<()> {
        // Build a dedicated one-shot HTTP client so that refresh uses
        // the same `insecure` behaviour as the gateway-client it's
        // backing, without having to reach back into whatever the
        // caller built at command construction time. Timeout is set
        // per-request below since build_http_client's defaults lean
        // permissive for long-running VNC / changefeed streams.
        let http = triton_tls::build_http_client(self.insecure)
            .await
            .map_err(|e| anyhow!("building HTTP client for refresh: {e}"))?;

        let url = format!("{}/v1/auth/refresh", self.gateway_url.trim_end_matches('/'));
        let body = RefreshRequest {
            refresh_token: state.refresh_token.clone(),
        };

        let response = http
            .post(&url)
            .timeout(REFRESH_HTTP_TIMEOUT)
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "refresh token rejected by gateway (HTTP {}): {}. \
                 Run `triton login` to re-authenticate.",
                status,
                text.trim()
            ));
        }

        #[derive(serde::Deserialize)]
        struct RefreshBody {
            token: String,
            refresh_token: String,
        }
        let body: RefreshBody = response
            .json()
            .await
            .context("parsing /v1/auth/refresh response body")?;

        let expires_at = jwt::extract_exp(&body.token)?;
        state.access_token = body.token;
        state.refresh_token = body.refresh_token;
        state.expires_at = expires_at;
        state.save(&self.profile).await?;

        Ok(())
    }
}

#[async_trait]
impl TokenProvider for FileTokenProvider {
    async fn current_token(&self) -> anyhow::Result<String> {
        let mut state = self.state.lock().await;

        // Proactive refresh: if we're within the refresh window, try to
        // rotate. On failure, return the existing (soon-to-expire) token
        // and let the caller's request attempt proceed. Failing
        // current_token() on a transient refresh glitch would be too
        // fragile.
        let seconds_left = (state.expires_at - Utc::now()).num_seconds();
        if seconds_left < PROACTIVE_REFRESH_WINDOW_SECS
            && let Err(e) = self.do_refresh(&mut state).await
        {
            tracing::debug!(
                "proactive refresh for profile '{}' failed: {e}",
                self.profile
            );
        }

        Ok(state.access_token.clone())
    }

    async fn on_unauthorized(&self) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        self.do_refresh(&mut state).await
    }
}
