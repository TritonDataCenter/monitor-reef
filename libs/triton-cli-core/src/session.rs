// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-command session + credential resolution, shared by both CLIs.
//!
//! Credential resolution order, highest priority first:
//!
//! 1. `--api-key` flag.
//! 2. `<PREFIX>_API_KEY` environment variable.
//! 3. `<PREFIX>_ACCESS_TOKEN` environment variable (no auto-refresh).
//! 4. Tokens stored in the config file. The access token is silently
//!    refreshed via `/v1/auth/refresh` if within [`REFRESH_LEEWAY`] of
//!    expiry, and the rewritten tokens are persisted back to disk.
//!
//! Endpoint resolution mirrors the same chain (`--endpoint`,
//! `<PREFIX>_ENDPOINT`, then config).
//!
//! This module is deliberately client-agnostic: it resolves the bearer
//! and endpoint and hands back a configured `reqwest::Client` via
//! [`Session::http_client`]. Each binary constructs its own typed
//! API client on top, so the eventual tenant/operator client split does
//! not reach into shared code.

use std::env;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::App;
use crate::config::{Config, Tokens};
use crate::http::build_http_client;

/// Refresh tokens within this window of expiring. 60s leaves headroom
/// for the upcoming request to complete even on a slow link.
const REFRESH_LEEWAY: Duration = Duration::seconds(60);

/// Resolved per-command session: the endpoint plus the bearer to send
/// (if any). The on-disk config has already been refreshed and
/// persisted by the time `resolve` returns.
pub struct Session {
    pub endpoint: String,
    pub bearer: Option<String>,
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    access_expires_at: DateTime<Utc>,
    refresh_expires_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
}

/// Exchange a username + password at `/v1/auth/login` for a fresh token
/// pair. Client-agnostic (raw POST); callers persist the result via
/// [`crate::Config`].
pub async fn login(endpoint: &str, username: &str, password: &str) -> Result<Tokens> {
    let http = build_http_client(None)?;
    let url = format!("{}/v1/auth/login", endpoint.trim_end_matches('/'));
    let resp: TokenResponse = http
        .post(&url)
        .json(&LoginRequest { username, password })
        .send()
        .await
        .with_context(|| format!("login against {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("login against {endpoint}"))?
        .json()
        .await
        .context("parse login response")?;
    Ok(Tokens {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        access_expires_at: resp.access_expires_at,
        refresh_expires_at: resp.refresh_expires_at,
    })
}

impl Session {
    /// Resolve credentials and endpoint for `app`, refreshing the
    /// stored access token if it is about to expire.
    pub async fn resolve(
        app: &App,
        endpoint_override: Option<String>,
        api_key_override: Option<String>,
    ) -> Result<Self> {
        let stored = Config::load(app).context("load config")?;

        let endpoint = endpoint_override
            .or_else(|| env::var(app.env("ENDPOINT")).ok())
            .or_else(|| stored.as_ref().map(|c| c.endpoint.clone()))
            .with_context(|| {
                format!(
                    "no endpoint configured: pass --endpoint, set {}, or run `{} configure`",
                    app.env("ENDPOINT"),
                    app.name,
                )
            })?;

        // Static credentials short-circuit the stored-token flow.
        if let Some(key) = api_key_override.or_else(|| env::var(app.env("API_KEY")).ok()) {
            return Ok(Self {
                endpoint,
                bearer: Some(key),
            });
        }
        if let Ok(access) = env::var(app.env("ACCESS_TOKEN")) {
            return Ok(Self {
                endpoint,
                bearer: Some(access),
            });
        }

        let mut config = match stored {
            Some(c) => c,
            None => {
                return Ok(Self {
                    endpoint,
                    bearer: None,
                });
            }
        };

        let bearer = match config.tokens.clone() {
            Some(tokens) => Some(ensure_fresh(app, &endpoint, &mut config, tokens).await?),
            None => None,
        };

        Ok(Self { endpoint, bearer })
    }

    /// A `reqwest::Client` carrying this session's bearer (if any) and
    /// the SmartOS-safe trust store.
    pub fn http_client(&self) -> Result<reqwest::Client> {
        build_http_client(self.bearer.as_deref())
    }
}

/// Refresh the stored access token if it is close to expiring,
/// rewriting the config on success. Returns the access token to use.
async fn ensure_fresh(
    app: &App,
    endpoint: &str,
    config: &mut Config,
    tokens: Tokens,
) -> Result<String> {
    if tokens.access_expires_at > Utc::now() + REFRESH_LEEWAY {
        return Ok(tokens.access_token);
    }
    if tokens.refresh_expires_at <= Utc::now() {
        anyhow::bail!(
            "refresh token has expired; run `{} login` to re-authenticate",
            app.name,
        );
    }

    let http = build_http_client(None)?;
    let url = format!("{}/v1/auth/refresh", endpoint.trim_end_matches('/'));
    let resp: TokenResponse = http
        .post(&url)
        .json(&RefreshRequest {
            refresh_token: &tokens.refresh_token,
        })
        .send()
        .await
        .context("refresh access token")?
        .error_for_status()
        .context("refresh access token")?
        .json()
        .await
        .context("parse refresh response")?;

    let fresh = Tokens {
        access_token: resp.access_token.clone(),
        refresh_token: resp.refresh_token,
        access_expires_at: resp.access_expires_at,
        refresh_expires_at: resp.refresh_expires_at,
    };
    config.tokens = Some(fresh);
    config.save(app).context("persist refreshed tokens")?;
    Ok(resp.access_token)
}
