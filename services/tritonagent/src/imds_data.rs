// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Realized-view data source for the IMDS daemon.
//!
//! The IMDS HTTP handlers don't reach into tritond (or FDB) directly
//! -- they go through a [`RealizedDataSource`] trait. Today this has
//! one implementation, [`TritondRealizedDataSource`], which calls
//! `GET /v2/instances/{id}/realized-meta`. Per `IMDS_DESIGN.md` (the
//! "swappable data-source" decision in §3 / the hot-path-independence
//! decision in the proposed-decisions table), the daemon's HTTP
//! surface doesn't change when the impl swaps -- a follow-up commit
//! can wire a direct restricted FDB-read impl alongside, keying off
//! the four `meta/gen/<scope>/<id>` counters, so IMDS keeps serving
//! while tritond is degraded.
//!
//! This module is just the trait + the tritond-backed impl + the
//! error type; the in-process cache that wraps both lives next.

use async_trait::async_trait;
use tritond_client::Client;
use tritond_client::types::RealizedMetaEntry;
use uuid::Uuid;

/// What can go wrong fetching one instance's realized view.
#[derive(Debug)]
pub enum RealizedFetchError {
    /// The instance UUID isn't known (or the principal isn't allowed
    /// to see it). Maps to a 404 / 403 on the IMDS side -- but the
    /// IMDS daemon never has a notion of "principal"; this should
    /// fire only if the proteus binding-table lookup returned a
    /// stale instance_id that no longer exists in tritond.
    NotFound,

    /// Anything else (transport, auth-against-tritond, JSON decode,
    /// ...). The daemon surfaces this as 503 -- the realized view
    /// is temporarily unavailable, retry. tritond going away should
    /// never block a guest from getting a *cached* answer (that's
    /// the cache wrapper's job, landing next).
    Backend(String),
}

impl std::fmt::Display for RealizedFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RealizedFetchError::NotFound => f.write_str("instance not found in tritond"),
            RealizedFetchError::Backend(s) => write!(f, "realized view unavailable: {s}"),
        }
    }
}

impl std::error::Error for RealizedFetchError {}

/// Source of one instance's full realized view: the precedence merge
/// of the four metadata scopes plus the computed system keys, each
/// leaf tagged with its provenance. See
/// `tritond_client::types::RealizedMetaEntry`.
#[async_trait]
pub trait RealizedDataSource: Send + Sync + 'static {
    /// Fetch the realized view for `instance_id`. Implementations
    /// MAY block on a network round trip; the IMDS daemon wraps this
    /// in a cache so the hot path doesn't pay that cost per request.
    async fn get(&self, instance_id: Uuid) -> Result<Vec<RealizedMetaEntry>, RealizedFetchError>;
}

/// tritond-backed implementation: calls
/// `GET /v2/instances/{id}/realized-meta` via the generated client.
/// The agent's existing API key (scope: `Agent`) authorises the
/// call; tritond's `MetaList` Cedar grant covers it (agent ==
/// tenant-member in the legacy sense for this CN's hosted
/// instances).
pub struct TritondRealizedDataSource {
    client: Client,
}

impl TritondRealizedDataSource {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl RealizedDataSource for TritondRealizedDataSource {
    async fn get(&self, instance_id: Uuid) -> Result<Vec<RealizedMetaEntry>, RealizedFetchError> {
        let response = self
            .client
            .get_instance_realized_meta()
            .instance_id(instance_id)
            .send()
            .await
            .map_err(|e| {
                // Progenitor's `Error` doesn't expose a stable
                // status-code variant, so we string-match the
                // `Display` for "not found" (404) and fall back to
                // `Backend(_)` for everything else. Coarse but
                // matches what every other tritonagent client call
                // is already doing.
                let msg = e.to_string();
                if msg.contains("404") || msg.to_ascii_lowercase().contains("not found") {
                    RealizedFetchError::NotFound
                } else {
                    RealizedFetchError::Backend(msg)
                }
            })?;
        Ok(response.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A no-op source for tests + the IMDS daemon's unit tests, so
    /// we don't need to stand up a fake tritond. Returns whatever
    /// we hand it.
    pub struct StaticRealizedDataSource(pub Vec<RealizedMetaEntry>);

    #[async_trait]
    impl RealizedDataSource for StaticRealizedDataSource {
        async fn get(&self, _: Uuid) -> Result<Vec<RealizedMetaEntry>, RealizedFetchError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn static_source_returns_what_it_holds() {
        let s = StaticRealizedDataSource(vec![]);
        let r = s.get(Uuid::nil()).await.unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn realized_fetch_error_display_is_stable() {
        let e = RealizedFetchError::NotFound;
        assert_eq!(format!("{e}"), "instance not found in tritond");
        let e = RealizedFetchError::Backend("boom".into());
        assert_eq!(format!("{e}"), "realized view unavailable: boom");
    }
}
