// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Cached JWKS consumer for verifying tritonapi-issued JWTs in external
//! services (triton-gateway, future adminui proxy, …).
//!
//! [`JwksClient`] holds a process-local map keyed by `kid`. It refreshes
//! lazily: a `verify_token` call whose `kid` isn't in the cache triggers
//! one refetch and retries. No background refresher — kid rotation ripples
//! through naturally on the next request, and token verification never
//! blocks on a periodic tick.

use crate::error::{SessionError, SessionResult};
use crate::jwt::JwtVerifier;
use crate::models::Claims;
use jsonwebtoken::decode_header;
use jsonwebtoken::jwk::JwkSet;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

pub struct JwksClient {
    http: reqwest::Client,
    jwks_url: String,
    verifiers: RwLock<HashMap<String, JwtVerifier>>,
}

impl JwksClient {
    pub fn new(jwks_url: impl Into<String>, http: reqwest::Client) -> Arc<Self> {
        Arc::new(Self {
            http,
            jwks_url: jwks_url.into(),
            verifiers: RwLock::new(HashMap::new()),
        })
    }

    /// Fetch the JWKS document from the upstream and replace the cached
    /// verifier map. Used at startup to prime the cache and on kid misses.
    pub async fn refresh(&self) -> SessionResult<()> {
        debug!("refreshing JWKS from {}", self.jwks_url);
        let resp = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .map_err(|e| SessionError::JwtKeyError(format!("JWKS fetch: {e}")))?
            .error_for_status()
            .map_err(|e| SessionError::JwtKeyError(format!("JWKS fetch status: {e}")))?;
        let set: JwkSet = resp
            .json()
            .await
            .map_err(|e| SessionError::JwtKeyError(format!("JWKS parse: {e}")))?;

        let mut new_map = HashMap::new();
        for jwk in &set.keys {
            let Some(kid) = jwk.common.key_id.as_ref() else {
                warn!("JWKS entry missing 'kid'; skipping");
                continue;
            };
            let verifier = JwtVerifier::from_jwk(jwk)?;
            new_map.insert(kid.clone(), verifier);
        }
        debug!("JWKS refresh loaded {} key(s)", new_map.len());
        *self.verifiers.write().await = new_map;
        Ok(())
    }

    async fn verifier_for(&self, kid: &str) -> Option<JwtVerifier> {
        self.verifiers.read().await.get(kid).cloned()
    }

    /// Verify a JWT. Looks up the signing kid from the token header; on
    /// cache miss, triggers one JWKS refresh and retries once before
    /// giving up with `InvalidToken`.
    pub async fn verify_token(&self, token: &str) -> SessionResult<Claims> {
        let header = decode_header(token).map_err(|_| SessionError::InvalidToken)?;
        let Some(kid) = header.kid else {
            return Err(SessionError::InvalidToken);
        };

        if let Some(verifier) = self.verifier_for(&kid).await {
            return verifier.verify_token(token);
        }

        // Unknown kid — could be a post-rotation key. Refetch once.
        self.refresh().await?;
        match self.verifier_for(&kid).await {
            Some(verifier) => verifier.verify_token(token),
            None => {
                warn!("JWT kid {kid} not present even after JWKS refresh");
                Err(SessionError::InvalidToken)
            }
        }
    }
}
