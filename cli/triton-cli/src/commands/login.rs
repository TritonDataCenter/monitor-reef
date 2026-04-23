// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `triton login` — exchange credentials for a tritonapi JWT.
//!
//! Two flows, sharing the same token-storage tail:
//!
//!   * **SSH-key login** (default): the profile's SSH key signs a
//!     request to `POST /v1/auth/login-ssh`. The server verifies the
//!     signature against the account's keys in mahi and issues a JWT
//!     pair.
//!   * **Password login** (`--user <login>` or a profile with no
//!     keyId): prompts for the LDAP password, then calls
//!     `POST /v1/auth/login` which binds against UFDS directly.
//!
//! Either path produces the same `LoginResponse`, which we stash at
//! `~/.triton/tokens/<profile>.json` (mode 0600, atomic write) so
//! subsequent commands can present the JWT as Bearer. The token file
//! lives outside the profile file intentionally -- older CLIs won't
//! trip over unfamiliar fields, and a future Keychain / libsecret
//! backend can replace the file storage without churning the profile
//! format.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use clap::Args;
use serde::{Deserialize, Serialize};
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::LoginResponse;

use crate::config::{Profile, paths};

/// `TokenProvider` that hands out a fixed access token loaded from
/// the profile's cached token file. No refresh logic -- if the
/// gateway returns 401 we bubble up an error telling the user to
/// re-run `triton login`. A richer refresh/rotation flow can slot
/// in later without changing call sites.
pub struct CachedTokenProvider {
    token: String,
}

impl CachedTokenProvider {
    pub fn new(token: String) -> Self {
        Self { token }
    }
}

#[async_trait::async_trait]
impl triton_gateway_client::TokenProvider for CachedTokenProvider {
    async fn current_token(&self) -> anyhow::Result<String> {
        Ok(self.token.clone())
    }

    async fn on_unauthorized(&self) -> anyhow::Result<()> {
        // Fail closed: the triton-gateway-client crate doesn't
        // actually invoke this today, but if it ever does, we want
        // the caller to see a clear "re-login" message rather than
        // silently retrying with the same stale token.
        Err(anyhow!(
            "cached token was rejected; run `triton login` to refresh"
        ))
    }
}

/// Best-effort load of cached tokens for a profile. Returns `None`
/// if there's no token file, the file is malformed, or the JWT
/// appears to be expired. Pure local read; callers that want to
/// attempt a refresh on expiry should use [`load_or_refresh`].
///
/// We don't verify the JWT's signature on read -- we trust the
/// file because we wrote it ourselves with mode 0600. The exp-claim
/// check is just to avoid presenting an obviously-stale token.
pub async fn load_if_fresh(profile_name: &str) -> Option<StoredTokens> {
    let path = paths::token_path(profile_name).ok()?;
    let contents = tokio::fs::read_to_string(&path).await.ok()?;
    let tokens: StoredTokens = serde_json::from_str(&contents).ok()?;
    if is_jwt_expired(&tokens.token) {
        return None;
    }
    Some(tokens)
}

/// Like [`load_if_fresh`], but if the access JWT is expired, attempt
/// a `POST /v1/auth/refresh` using the stored refresh token. On success
/// the rotated `(token, refresh_token)` pair is persisted atomically
/// before the updated `StoredTokens` are returned, so the next
/// invocation sees a fresh file without re-refreshing. Returns `None`
/// on any failure (no token file, malformed file, refresh 401, network
/// down) and the caller falls through to SSH auth.
pub async fn load_or_refresh(
    profile_name: &str,
    gateway_url: &str,
    insecure: bool,
) -> Option<StoredTokens> {
    let path = paths::token_path(profile_name).ok()?;
    let contents = tokio::fs::read_to_string(&path).await.ok()?;
    let mut tokens: StoredTokens = serde_json::from_str(&contents).ok()?;
    if !is_jwt_expired(&tokens.token) {
        return Some(tokens);
    }
    // Access JWT expired. Try the refresh endpoint; if the stored
    // refresh_token has also been consumed or expired, the server will
    // 401 and we fall through to SSH. Refresh tokens are single-use,
    // so a concurrent `triton` invocation winning the race against us
    // shows up as the same 401 -- also fine.
    let http = crate::build_http_client(insecure).await.ok()?;
    let refreshed = call_refresh(&http, gateway_url, &tokens.refresh_token)
        .await
        .ok()?;
    tokens.token = refreshed.token;
    tokens.refresh_token = refreshed.refresh_token;
    tokens.issued_at = chrono::Utc::now().timestamp();
    write_tokens(profile_name, &tokens).await.ok()?;
    Some(tokens)
}

/// POST the stored `refresh_token` to `/v1/auth/refresh` and parse the
/// returned `(token, refresh_token)` pair. Extracted so tests can drive
/// it against a stub HTTP server without touching the filesystem.
async fn call_refresh(
    http: &reqwest::Client,
    gateway_url: &str,
    refresh_token: &str,
) -> Result<RefreshedPair> {
    let url = format!("{}/v1/auth/refresh", gateway_url.trim_end_matches('/'));
    let resp = http
        .post(&url)
        .json(&serde_json::json!({ "refresh_token": refresh_token }))
        .send()
        .await
        .map_err(|e| anyhow!("refresh request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("refresh endpoint returned {}", resp.status()));
    }
    let parsed: RefreshedPair = resp
        .json()
        .await
        .map_err(|e| anyhow!("refresh response parse failed: {e}"))?;
    Ok(parsed)
}

#[derive(Debug, Deserialize)]
struct RefreshedPair {
    token: String,
    refresh_token: String,
}

/// Build a `GatewayAuthConfig::bearer` from a `StoredTokens` + the
/// account string to stamp into `/{account}/*` paths (typically the
/// profile's configured account). Separated so the main.rs wire-up
/// reads as "try cached, fall back to SSH" without knowing the
/// provider's concrete type.
pub fn bearer_auth_from(
    tokens: &StoredTokens,
    account: impl Into<String>,
) -> triton_gateway_client::GatewayAuthConfig {
    let provider = Arc::new(CachedTokenProvider::new(tokens.token.clone()));
    triton_gateway_client::GatewayAuthConfig::bearer(provider, account)
}

/// Decode a JWT's `exp` claim WITHOUT verifying the signature and
/// compare against the current time. Returns `true` if the token
/// looks expired (or malformed / unparseable) so the caller falls
/// back to SSH auth. We shave 30 seconds off the apparent expiry
/// so we don't present a token that will expire between our check
/// and the gateway's verification.
fn is_jwt_expired(jwt: &str) -> bool {
    use base64::Engine as _;
    let Some(payload_b64) = jwt.split('.').nth(1) else {
        return true;
    };
    let Ok(payload) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64) else {
        return true;
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return true;
    };
    // A JWT without `exp` is unusual but not wrong; assume it's still
    // valid and let the server reject it if not.
    let Some(exp) = claims.get("exp").and_then(|v| v.as_i64()) else {
        return false;
    };
    let now = chrono::Utc::now().timestamp();
    exp <= now + 30
}

#[derive(Args, Clone, Debug, Default)]
pub struct LoginArgs {
    /// Force password login for this login name. Without this flag,
    /// `triton login` uses the profile's SSH key to authenticate via
    /// `/v1/auth/login-ssh`. With it, prompts for the LDAP password
    /// and authenticates via `/v1/auth/login`. Useful when the profile
    /// has no keyId or the operator wants to exercise the password
    /// path explicitly.
    #[arg(short = 'u', long = "user")]
    pub user: Option<String>,
}

/// Persisted form of a successful `/v1/auth/login-ssh` exchange.
/// Written to `~/.triton/tokens/<profile>.json`. Deliberately flat
/// and stable -- any future backend (Keychain, etc.) can read/write
/// the same shape.
#[derive(Debug, Serialize, Deserialize)]
pub struct StoredTokens {
    pub token: String,
    pub refresh_token: String,
    pub username: String,
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub is_admin: bool,
    /// Unix epoch seconds at which these tokens were issued. Used by
    /// future proactive-refresh logic; today just informational.
    pub issued_at: i64,
}

pub async fn run(
    args: LoginArgs,
    client: &TypedClient,
    profile: &Profile,
    use_json: bool,
) -> Result<()> {
    let login = match args.user {
        Some(username) => password_login(client, &username).await?,
        None => ssh_login(client).await?,
    };

    let stored = StoredTokens {
        token: login.token.clone(),
        refresh_token: login.refresh_token.clone(),
        username: login.user.username.clone(),
        user_id: login.user.id.to_string(),
        email: login.user.email.clone(),
        is_admin: login.user.is_admin,
        issued_at: chrono::Utc::now().timestamp(),
    };

    write_tokens(&profile.name, &stored)
        .await
        .with_context(|| format!("failed to persist tokens for profile '{}'", profile.name))?;

    if use_json {
        // Emit the stored shape (minus the raw token value, which the
        // user can fetch from the file if they need it) so scripts can
        // confirm success without parsing prose.
        let summary = serde_json::json!({
            "profile": profile.name,
            "username": stored.username,
            "user_id": stored.user_id,
            "email": stored.email,
            "is_admin": stored.is_admin,
            "issued_at": stored.issued_at,
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("Logged in to {} as {}.", profile.url, stored.username);
        if stored.is_admin {
            println!("  (operator / admin)");
        }
        let path = paths::token_path(&profile.name)?;
        println!("Token saved to {}", path.display());
    }

    Ok(())
}

async fn ssh_login(client: &TypedClient) -> Result<LoginResponse> {
    let response = client
        .inner()
        .auth_login_ssh()
        .send()
        .await
        .map_err(|e| anyhow!("SSH login failed: {e}"))?;
    Ok(response.into_inner())
}

async fn password_login(client: &TypedClient, username: &str) -> Result<LoginResponse> {
    // `TRITON_PASSWORD` is an undocumented escape hatch for non-tty
    // flows (integration tests, scripted operator runbooks). An
    // interactive prompt is the primary UX.
    let password = match std::env::var("TRITON_PASSWORD").ok() {
        Some(p) => p,
        None => rpassword::prompt_password(format!("Password for {username}: "))
            .map_err(|e| anyhow!("failed to read password: {e}"))?,
    };
    let body = triton_gateway_client::types::LoginRequest {
        username: username.to_string(),
        password,
    };
    let response = client
        .inner()
        .auth_login()
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow!("password login failed: {e}"))?;
    Ok(response.into_inner())
}

/// Write tokens atomically with mode 0600. Create-parent-dir on
/// demand (mode 0700 on the directory). Uses a temp file + rename so
/// a concurrent login or crash can never produce a half-written
/// token file that subsequent Bearer calls would choke on.
async fn write_tokens(profile_name: &str, tokens: &StoredTokens) -> Result<()> {
    let path = paths::token_path(profile_name)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create tokens directory {}", parent.display()))?;
        set_mode_if_unix(parent, 0o700).await?;
    }
    let tmp = path.with_extension("json.new");
    let json = serde_json::to_vec_pretty(tokens)?;
    tokio::fs::write(&tmp, &json)
        .await
        .with_context(|| format!("write {}", tmp.display()))?;
    set_mode_if_unix(&tmp, 0o600).await?;
    tokio::fs::rename(&tmp, &path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(unix)]
async fn set_mode_if_unix(path: &std::path::Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
    tokio::fs::set_permissions(path, perms)
        .await
        .with_context(|| format!("chmod {:o} {}", mode, path.display()))
}

#[cfg(not(unix))]
async fn set_mode_if_unix(_: &std::path::Path, _: u32) -> Result<()> {
    // Non-unix (Windows dev builds): rely on the default ACL. Triton
    // deployments are illumos/Linux; this is a dev-ergonomics fallback.
    Ok(())
}

#[cfg(test)]
mod refresh_tests {
    //! Tests for the refresh wire path. We spin a minimal plain-HTTP
    //! server on a random localhost port and drive `call_refresh`
    //! against it -- cheaper than standing up the gateway, and the
    //! refresh endpoint itself is unauthenticated on the wire so we
    //! don't miss any TLS-specific behaviour by using plain HTTP.
    //!
    //! `reqwest` eagerly reaches for the default rustls crypto provider
    //! even for plain HTTP; `triton_tls::install_default_crypto_provider`
    //! is idempotent and installs the same provider the production CLI
    //! uses. The per-test call matches the pattern already established
    //! by `insecure_mode_accepts_self_signed_cert` in main.rs.
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn test_http_client() -> reqwest::Client {
        triton_tls::install_default_crypto_provider();
        reqwest::Client::new()
    }

    /// Spawn a one-shot HTTP server that accepts one connection,
    /// reads the request (discarded), and writes `response_body` with
    /// the given HTTP status. Returns the bound base URL. The server
    /// task ends after the single response.
    async fn one_shot_http(response_body: Vec<u8>, status: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf).await;
            let header = format!(
                "HTTP/1.1 {}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
                status,
                response_body.len()
            );
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(&response_body).await;
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn call_refresh_parses_valid_response() {
        let body = serde_json::to_vec(&serde_json::json!({
            "token": "new.access.jwt",
            "refresh_token": "new-refresh-token",
        }))
        .unwrap();
        let url = one_shot_http(body, "200 OK").await;
        let pair = call_refresh(&test_http_client(), &url, "stale-refresh-token")
            .await
            .expect("refresh should succeed");
        assert_eq!(pair.token, "new.access.jwt");
        assert_eq!(pair.refresh_token, "new-refresh-token");
    }

    #[tokio::test]
    async fn call_refresh_errors_on_401() {
        let url = one_shot_http(b"{\"error\":\"nope\"}".to_vec(), "401 Unauthorized").await;
        let err = call_refresh(&test_http_client(), &url, "revoked-refresh-token")
            .await
            .expect_err("401 should surface as Err");
        // Caller only cares that it's an Err (load_or_refresh maps any
        // Err to None); the exact message is informational.
        assert!(err.to_string().contains("401"), "got: {err}");
    }

    #[tokio::test]
    async fn call_refresh_errors_on_malformed_body() {
        let url = one_shot_http(b"not json".to_vec(), "200 OK").await;
        let err = call_refresh(&test_http_client(), &url, "whatever")
            .await
            .expect_err("malformed JSON should surface as Err");
        assert!(err.to_string().contains("parse"), "got: {err}");
    }

    #[tokio::test]
    async fn call_refresh_trims_trailing_slash_on_url() {
        // Regression: users (and profiles) sometimes have a trailing
        // slash on the gateway URL. Make sure we don't double up on
        // `/v1/auth/refresh` -> `//v1/auth/refresh` and 404.
        let body = serde_json::to_vec(&serde_json::json!({
            "token": "t",
            "refresh_token": "r",
        }))
        .unwrap();
        let url = one_shot_http(body, "200 OK").await;
        let url_with_slash = format!("{url}/");
        let pair = call_refresh(&test_http_client(), &url_with_slash, "x")
            .await
            .expect("trailing-slash URL should still work");
        assert_eq!(pair.token, "t");
    }
}
