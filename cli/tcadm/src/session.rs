// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Glue between [`crate::config::Config`] on disk, the runtime
//! credential a single command needs, and the
//! `tritond_client::Client` it talks through.
//!
//! Resolution order for credentials, highest priority first:
//!
//! 1. `--api-key` flag.
//! 2. `TCADM_API_KEY` environment variable.
//! 3. `TCADM_ACCESS_TOKEN` environment variable (no auto-refresh).
//! 4. Tokens stored in `~/.config/tcadm/config.json`. The access
//!    token is silently refreshed via `/v2/auth/refresh` if it has
//!    less than `REFRESH_LEEWAY` left, and the rewritten tokens are
//!    persisted back to disk.
//!
//! Endpoint resolution mirrors the same chain
//! (`--endpoint`, `TCADM_ENDPOINT`, then config).

use std::env;

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use tritond_client::Client;
use tritond_client::types::{RefreshRequest, TokenResponse};

use crate::config::{Config, Tokens};

/// Refresh tokens that are within this window of expiring. 60s gives
/// us enough headroom for the upcoming request to succeed even if
/// the network is slow.
const REFRESH_LEEWAY: Duration = Duration::seconds(60);

/// Resolved per-command session: endpoint plus the bearer header to
/// send (if any). The on-disk config has already been refreshed and
/// persisted by the time `resolve` returns, so subcommand handlers
/// don't need to think about it.
pub struct Session {
    pub endpoint: String,
    pub bearer: Option<String>,
}

impl Session {
    /// Load whatever the user has on disk plus environment / flag
    /// overrides, refreshing the stored access token if it's about
    /// to expire.
    pub async fn resolve(
        endpoint_override: Option<String>,
        api_key_override: Option<String>,
    ) -> Result<Self> {
        let stored = Config::load().context("load config")?;

        let endpoint = endpoint_override
            .or_else(|| env::var("TCADM_ENDPOINT").ok())
            .or_else(|| stored.as_ref().map(|c| c.endpoint.clone()))
            .context(
                "no endpoint configured: pass --endpoint, set TCADM_ENDPOINT, or run `tcadm configure`",
            )?;

        // Static credentials short-circuit the whole stored-token flow.
        if let Some(key) = api_key_override.or_else(|| env::var("TCADM_API_KEY").ok()) {
            return Ok(Self {
                endpoint,
                bearer: Some(key),
            });
        }
        if let Ok(access) = env::var("TCADM_ACCESS_TOKEN") {
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

        let bearer = if let Some(tokens) = config.tokens.clone() {
            Some(ensure_fresh(&endpoint, &mut config, tokens).await?)
        } else {
            None
        };

        Ok(Self { endpoint, bearer })
    }

    /// Build a `tritond_client::Client` configured with the bearer header.
    pub fn client(&self) -> Result<Client> {
        let http = build_http_client(self.bearer.as_deref())?;
        Ok(Client::new_with_client(&self.endpoint, http))
    }
}

/// Build a `reqwest::Client` that has rustls preconfigured with the
/// bundled `webpki_roots` trust store. SmartOS GZ has no system CA
/// bundle, so the platform verifier panics on startup; we ship our own
/// roots in the binary. Same posture as the tritonagent client.
///
/// `bearer` is optional; when set, an `Authorization: Bearer …` header
/// is attached as a default for every request the returned client makes.
pub fn build_http_client(bearer: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(bearer) = bearer {
        let value = format!("Bearer {bearer}")
            .parse()
            .context("invalid bearer token characters")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    reqwest::Client::builder()
        .default_headers(headers)
        .use_preconfigured_tls(tls)
        .build()
        .context("build reqwest client")
}

/// Build an anonymous (no bearer) `tritond_client::Client` against
/// `endpoint`. Goes through the same TLS config path as
/// [`Session::client`] — required on SmartOS GZ for the same reason.
pub fn anonymous_client(endpoint: &str) -> Result<Client> {
    let http = build_http_client(None)?;
    Ok(Client::new_with_client(endpoint, http))
}

/// Refresh the stored access token if it's close to expiring,
/// rewriting `~/.config/tcadm/config.json` on success. Returns the
/// access token (whether refreshed or still good) for use as a
/// bearer.
async fn ensure_fresh(endpoint: &str, config: &mut Config, tokens: Tokens) -> Result<String> {
    if tokens.access_expires_at > Utc::now() + REFRESH_LEEWAY {
        return Ok(tokens.access_token);
    }

    if tokens.refresh_expires_at <= Utc::now() {
        anyhow::bail!("refresh token has expired; run `tcadm login` to re-authenticate");
    }

    let anonymous = anonymous_client(endpoint)?;
    let response: TokenResponse = anonymous
        .refresh()
        .body(RefreshRequest {
            refresh_token: tokens.refresh_token.clone(),
        })
        .send()
        .await
        .context("refresh access token")?
        .into_inner();

    let fresh = Tokens {
        access_token: response.access_token.clone(),
        refresh_token: response.refresh_token,
        access_expires_at: response.access_expires_at,
        refresh_expires_at: response.refresh_expires_at,
    };
    config.tokens = Some(fresh);
    config.save().context("persist refreshed tokens")?;
    Ok(response.access_token)
}
