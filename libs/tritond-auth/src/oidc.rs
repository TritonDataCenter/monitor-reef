// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! OIDC ID-token verification with JWKS caching.
//!
//! Phase 0e-b only supports **direct ID-token presentation** —
//! callers send `Authorization: Bearer <id_token>` and we verify it
//! against the silo's configured IdP. Authorization Code Flow,
//! device-code flow, and the SAML path are out of scope for this
//! slice.
//!
//! # Cache shape
//!
//! [`OidcVerifier`] keeps a per-cache-key snapshot of an IdP's
//! discovered [`CoreProviderMetadata`] (which includes the JWKS).
//! Discovery happens lazily on first verify, eagerly via
//! [`OidcVerifier::discover`] when an operator POSTs an IdP config.
//! Cached entries expire after [`CACHE_TTL`] and the next call
//! re-fetches.
//!
//! # What we deliberately don't expose
//!
//! Errors come back as [`OidcError`] — opaque variants that don't
//! leak the underlying `openidconnect`/`oauth2`/`reqwest` error
//! types.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openidconnect::core::{CoreClient, CoreProviderMetadata};
use openidconnect::reqwest::Client as OidcReqwestClient;
use openidconnect::{ClientId, ClientSecret, IssuerUrl, Nonce};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// How long to cache a per-silo discovery + JWKS response. Long
/// enough to amortise the round trip across reasonable login load,
/// short enough that key rotation at the IdP is picked up within
/// minutes.
pub const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Connection timeout on outbound requests to the IdP. We don't
/// want a slow IdP to stall the auth path indefinitely.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration parameters extracted from a silo's IdP config.
/// Mirrors the fields of `tritond_store::IdpConfig`; we keep the
/// type local so this crate stays storage-free.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub audience: Option<String>,
}

/// Subset of an OIDC ID token's claims that the auth middleware
/// cares about. The full claim set survives in `claims_json` for
/// audit and policy use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcClaims {
    /// `iss` claim — must match the configured `issuer_url`.
    pub issuer: String,
    /// `sub` claim — the IdP's stable identifier for the user.
    pub subject: String,
    /// Best display username we can derive: `email` if present,
    /// else `preferred_username`, else `sub`.
    pub username: String,
    /// `email` claim if the IdP supplied one.
    pub email: Option<String>,
    /// Full claim set for audit / Cedar policy / future use.
    pub claims_json: serde_json::Value,
}

/// Errors returned by the OIDC verifier.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OidcError {
    /// Discovery document fetch failed (network, 4xx/5xx, or invalid
    /// JSON). The operator should re-check the IdP issuer URL.
    #[error("oidc discovery failed: {0}")]
    Discovery(String),

    /// The presented token was malformed (couldn't be parsed as a
    /// JWT) or missing required claims.
    #[error("oidc token malformed: {0}")]
    Malformed(String),

    /// The token's signature didn't verify against the IdP's JWKS.
    #[error("oidc signature mismatch")]
    BadSignature,

    /// The token had expired by the time we verified it, or its
    /// audience / issuer didn't match what was configured.
    #[error("oidc token rejected: {0}")]
    Rejected(String),

    /// Building the reqwest client used for outbound IdP calls failed
    /// (TLS configuration, etc.). Operator-fixable in principle.
    #[error("oidc http client init failed: {0}")]
    HttpClient(String),
}

/// Per-cluster OIDC verifier. Holds a cache of discovered IdP
/// metadata + JWKS keyed by silo identifier; thread-safe across the
/// async runtime.
pub struct OidcVerifier {
    cache: RwLock<HashMap<String, CacheEntry>>,
}

#[derive(Clone)]
struct CacheEntry {
    metadata: CoreProviderMetadata,
    client_id: String,
    client_secret: String,
    issuer_url: String,
    audience: Option<String>,
    fetched_at: Instant,
}

impl OidcVerifier {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Eagerly fetch the discovery document and populate the cache.
    /// Used by `POST /v1/silos/{silo_id}/idp` so a misconfigured IdP
    /// fails the write rather than producing mysterious 401s on
    /// later login attempts.
    pub async fn discover(&self, cache_key: &str, config: &OidcConfig) -> Result<(), OidcError> {
        let entry = fetch(config).await?;
        let mut cache = self.cache.write().await;
        cache.insert(cache_key.to_string(), entry);
        Ok(())
    }

    /// Drop a cached entry — used when an IdP config changes or is
    /// deleted so the next request re-discovers.
    pub async fn invalidate(&self, cache_key: &str) {
        let mut cache = self.cache.write().await;
        cache.remove(cache_key);
    }

    /// Verify a presented ID token against `config`. The cache is
    /// consulted first; on miss or expiry, discovery runs again.
    pub async fn verify(
        &self,
        cache_key: &str,
        config: &OidcConfig,
        id_token: &str,
    ) -> Result<OidcClaims, OidcError> {
        let entry = self.get_or_fetch(cache_key, config).await?;

        let client = CoreClient::from_provider_metadata(
            entry.metadata,
            ClientId::new(entry.client_id),
            Some(ClientSecret::new(entry.client_secret)),
        );

        use openidconnect::core::CoreIdToken;
        let parsed = id_token
            .parse::<CoreIdToken>()
            .map_err(|e| OidcError::Malformed(e.to_string()))?;

        let verifier = client.id_token_verifier();
        let claims = parsed
            .claims(&verifier, no_nonce_required)
            .map_err(map_claims_error)?;

        let issuer = claims.issuer().to_string();
        if issuer != entry.issuer_url {
            return Err(OidcError::Rejected(format!(
                "issuer claim {issuer:?} != configured {:?}",
                entry.issuer_url
            )));
        }
        if let Some(expected_aud) = entry.audience.as_deref() {
            let matched = claims
                .audiences()
                .iter()
                .any(|a| a.as_str() == expected_aud);
            if !matched {
                return Err(OidcError::Rejected(format!(
                    "audience claim does not contain {expected_aud:?}"
                )));
            }
        }

        let subject = claims.subject().to_string();
        let email = claims
            .email()
            .map(|e| e.as_str().to_string())
            .filter(|s| !s.is_empty());
        let preferred_username = claims
            .preferred_username()
            .map(|p| p.as_str().to_string())
            .filter(|s| !s.is_empty());
        let username = email
            .clone()
            .or(preferred_username)
            .unwrap_or_else(|| subject.clone());

        let claims_json = serde_json::to_value(claims)
            .map_err(|e| OidcError::Malformed(format!("claims to json: {e}")))?;

        Ok(OidcClaims {
            issuer,
            subject,
            username,
            email,
            claims_json,
        })
    }

    async fn get_or_fetch(
        &self,
        cache_key: &str,
        config: &OidcConfig,
    ) -> Result<CacheEntry, OidcError> {
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(cache_key)
                && entry.fetched_at.elapsed() < CACHE_TTL
                && entry.issuer_url == config.issuer_url
            {
                return Ok(entry.clone());
            }
        }
        let entry = fetch(config).await?;
        let mut cache = self.cache.write().await;
        cache.insert(cache_key.to_string(), entry.clone());
        Ok(entry)
    }
}

impl Default for OidcVerifier {
    fn default() -> Self {
        Self::new()
    }
}

async fn fetch(config: &OidcConfig) -> Result<CacheEntry, OidcError> {
    let http_client = OidcReqwestClient::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| OidcError::HttpClient(e.to_string()))?;

    let issuer = IssuerUrl::new(config.issuer_url.clone())
        .map_err(|e| OidcError::Discovery(format!("issuer url: {e}")))?;
    let metadata = CoreProviderMetadata::discover_async(issuer, &http_client)
        .await
        .map_err(|e| OidcError::Discovery(e.to_string()))?;
    Ok(CacheEntry {
        metadata,
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.clone(),
        issuer_url: config.issuer_url.clone(),
        audience: config.audience.clone(),
        fetched_at: Instant::now(),
    })
}

/// Closure satisfying openidconnect's nonce-verification slot when
/// no nonce was negotiated (we don't run the Authorization Code Flow).
fn no_nonce_required(_: Option<&Nonce>) -> Result<(), String> {
    Ok(())
}

fn map_claims_error(err: openidconnect::ClaimsVerificationError) -> OidcError {
    use openidconnect::ClaimsVerificationError;
    match err {
        ClaimsVerificationError::SignatureVerification(_) => OidcError::BadSignature,
        ClaimsVerificationError::Expired(_) => OidcError::Rejected("token expired".to_string()),
        ClaimsVerificationError::InvalidAudience(_) => {
            OidcError::Rejected("invalid audience".to_string())
        }
        ClaimsVerificationError::InvalidIssuer(_) => {
            OidcError::Rejected("invalid issuer".to_string())
        }
        other => OidcError::Rejected(other.to_string()),
    }
}

/// Peek at the `iss` claim of an ID token without verifying its
/// signature. The auth middleware uses this to find the silo whose
/// IdP config to verify against.
///
/// Returns `None` for any malformed or missing-iss token. Callers
/// must still verify the token afterwards — this is purely a
/// routing hint.
#[must_use]
pub fn peek_issuer(id_token: &str) -> Option<String> {
    use base64::Engine;
    let payload = id_token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get("iss")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[tokio::test]
    async fn peek_issuer_extracts_iss_claim() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"iss":"https://idp.example","sub":"abc"}"#);
        let token = format!("{header}.{payload}.signature");
        assert_eq!(peek_issuer(&token).as_deref(), Some("https://idp.example"));
    }

    #[tokio::test]
    async fn peek_issuer_returns_none_for_garbage() {
        assert!(peek_issuer("not.a.token").is_none());
        assert!(peek_issuer("").is_none());
    }

    #[tokio::test]
    async fn discover_against_unreachable_url_fails_fast() {
        let verifier = OidcVerifier::new();
        let config = OidcConfig {
            issuer_url: "http://127.0.0.1:1".to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        };
        let err = verifier
            .discover("silo-test", &config)
            .await
            .expect_err("unreachable IdP must fail discovery");
        assert!(matches!(err, OidcError::Discovery(_)));
    }
}
