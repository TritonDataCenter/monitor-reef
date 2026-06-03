// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Where the verifier gets a realm's public keys. Production uses
//! [`PollingJwksSource`] (a `kid`-indexed, TTL'd HTTP cache); tests
//! inject a [`StaticJwksSource`]. A cache miss on an unseen `kid`
//! forces one refresh, so key rotation is picked up without a restart.

use std::sync::RwLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::jwk::{Jwk, JwkSet};

/// Failure fetching or parsing a JWK set.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JwksError {
    #[error("jwks fetch failed: {0}")]
    Fetch(String),
    #[error("jwks parse failed: {0}")]
    Parse(String),
}

/// Resolves a JWK by `kid`. `Ok(None)` means "no such key after a fresh
/// look" (so the verifier returns `UnknownKid`); `Err` means the source
/// itself is unavailable.
#[async_trait]
pub trait JwksSource: Send + Sync {
    async fn jwk_for_kid(&self, kid: &str) -> Result<Option<Jwk>, JwksError>;
}

/// Fixed key set, never refreshed. For tests and pinned deployments.
pub struct StaticJwksSource {
    set: JwkSet,
}

impl StaticJwksSource {
    #[must_use]
    pub fn new(set: JwkSet) -> Self {
        Self { set }
    }

    /// Parse a JWK set from JSON (`{"keys":[...]}`).
    pub fn from_json(json: &str) -> Result<Self, JwksError> {
        let set: JwkSet = serde_json::from_str(json).map_err(|e| JwksError::Parse(e.to_string()))?;
        Ok(Self { set })
    }
}

#[async_trait]
impl JwksSource for StaticJwksSource {
    async fn jwk_for_kid(&self, kid: &str) -> Result<Option<Jwk>, JwksError> {
        Ok(self.set.find(kid).cloned())
    }
}

struct Cached {
    set: JwkSet,
    fetched: Instant,
}

/// Polls a JWKS URL over HTTP, caching the set for `ttl`. A request for
/// an unknown `kid` (or a stale cache) triggers a refresh.
pub struct PollingJwksSource {
    url: String,
    client: reqwest::Client,
    ttl: Duration,
    cache: RwLock<Option<Cached>>,
}

impl PollingJwksSource {
    /// New source for a realm's `.../jwks` URL with the given cache TTL.
    #[must_use]
    pub fn new(url: impl Into<String>, ttl: Duration) -> Self {
        Self {
            url: url.into(),
            client: reqwest::Client::new(),
            ttl,
            cache: RwLock::new(None),
        }
    }

    /// Use a caller-provided client (shared connection pool, custom TLS).
    #[must_use]
    pub fn with_client(url: impl Into<String>, ttl: Duration, client: reqwest::Client) -> Self {
        Self {
            url: url.into(),
            client,
            ttl,
            cache: RwLock::new(None),
        }
    }

    async fn refresh(&self) -> Result<JwkSet, JwksError> {
        let resp = self
            .client
            .get(&self.url)
            .send()
            .await
            .map_err(|e| JwksError::Fetch(e.to_string()))?
            .error_for_status()
            .map_err(|e| JwksError::Fetch(e.to_string()))?;
        let set: JwkSet = resp
            .json()
            .await
            .map_err(|e| JwksError::Parse(e.to_string()))?;
        // PoisonError just means a writer panicked; the data is still
        // sound to overwrite, so recover the guard rather than unwrap.
        let mut guard = self.cache.write().unwrap_or_else(|e| e.into_inner());
        *guard = Some(Cached {
            set: set.clone(),
            fetched: Instant::now(),
        });
        Ok(set)
    }
}

#[async_trait]
impl JwksSource for PollingJwksSource {
    async fn jwk_for_kid(&self, kid: &str) -> Result<Option<Jwk>, JwksError> {
        // Fast path: a fresh cache that already has the key. The guard
        // is dropped before any await (std RwLock guards are not Send).
        {
            let guard = self.cache.read().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = guard.as_ref() {
                if c.fetched.elapsed() < self.ttl {
                    if let Some(jwk) = c.set.find(kid) {
                        return Ok(Some(jwk.clone()));
                    }
                }
            }
        }
        // Stale, empty, or unknown kid: refresh once and look again.
        let set = self.refresh().await?;
        Ok(set.find(kid).cloned())
    }
}
