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
    pub(crate) struct StaticRealizedDataSource(pub(crate) Vec<RealizedMetaEntry>);

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

// =============================================================================
// In-process realized-view cache
// =============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

/// Default cache TTL: cache-served reads stay valid for this long
/// before the daemon re-fetches. The design's target is "no TTL --
/// invalidate driven by an FDB watch on the four `meta/gen/<scope>/<id>`
/// counters" (see `IMDS_DESIGN.md` §1.4, §3); until that watch is
/// wired, this TTL is the bounded staleness window. Writeback on
/// `triton/guest/*` calls `invalidate()` to bust the cache for the
/// affected instance immediately.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(30);

/// One cached realized view + the wall-clock instant it was
/// fetched. `entries` is wrapped in `Arc` so a hot read returns
/// the same allocation to N concurrent handlers without cloning
/// the whole Vec.
#[derive(Clone)]
struct CachedView {
    entries: Arc<Vec<RealizedMetaEntry>>,
    fetched_at: Instant,
}

/// In-process cache wrapping a [`RealizedDataSource`]. Cheap to
/// clone (`Arc` inside). The IMDS daemon holds one of these on its
/// state and calls `get()` on every request; cache-hits return
/// immediately, cache-misses populate the entry.
///
/// Concurrency caveat: two simultaneous misses for the same key
/// each do their own fetch. That's a perf footgun rather than a
/// correctness one -- the design promises eventual consistency,
/// not single-flight -- and the singleflight wrapper is the same
/// shape we'd retrofit once the per-instance request rate
/// justifies it.
#[derive(Clone)]
pub struct RealizedViewCache<D: RealizedDataSource> {
    inner: Arc<RealizedViewCacheInner<D>>,
}

struct RealizedViewCacheInner<D: RealizedDataSource> {
    source: D,
    ttl: Duration,
    by_instance: RwLock<HashMap<Uuid, CachedView>>,
}

impl<D: RealizedDataSource> RealizedViewCache<D> {
    /// New cache with the [`DEFAULT_CACHE_TTL`].
    pub fn new(source: D) -> Self {
        Self::with_ttl(source, DEFAULT_CACHE_TTL)
    }

    /// New cache with a custom TTL (tests use a tiny TTL to exercise
    /// the expiry path; production uses the default).
    pub fn with_ttl(source: D, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(RealizedViewCacheInner {
                source,
                ttl,
                by_instance: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Fetch (or return cached) the realized view for `instance_id`.
    /// On cache miss, the upstream `RealizedDataSource::get` is
    /// awaited; if it fails, the error propagates without touching
    /// the cached entry (a stale-cached value is preferable to
    /// failing every guest read when tritond hiccups).
    pub async fn get(
        &self,
        instance_id: Uuid,
    ) -> Result<Arc<Vec<RealizedMetaEntry>>, RealizedFetchError> {
        // Read path: cache hit returns immediately.
        {
            let g = self.inner.by_instance.read().await;
            if let Some(cached) = g.get(&instance_id)
                && cached.fetched_at.elapsed() < self.inner.ttl
            {
                return Ok(cached.entries.clone());
            }
        }
        // Miss path: drop the read guard, fetch, take the write
        // guard, insert. A racing reader between the read drop and
        // the write acquire may also fetch -- harmless duplication.
        let entries = Arc::new(self.inner.source.get(instance_id).await?);
        let mut g = self.inner.by_instance.write().await;
        g.insert(
            instance_id,
            CachedView {
                entries: entries.clone(),
                fetched_at: Instant::now(),
            },
        );
        Ok(entries)
    }

    /// Drop the cached view for `instance_id`. Called by the
    /// writeback path on `triton/guest/*` PUT/DELETE so the very
    /// next read sees the post-write value, and by future
    /// gen-tuple-watch wakeups when a scope counter advances.
    pub async fn invalidate(&self, instance_id: Uuid) {
        let mut g = self.inner.by_instance.write().await;
        g.remove(&instance_id);
    }

    /// Drop the entire cache. Diagnostics + tests.
    pub async fn clear(&self) {
        let mut g = self.inner.by_instance.write().await;
        g.clear();
    }
}

#[cfg(test)]
mod cache_tests {
    use super::tests::StaticRealizedDataSource;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tritond_client::types::{MetaProvenance, MetaValue};

    fn entry(key: &str) -> RealizedMetaEntry {
        RealizedMetaEntry {
            key: key.to_string(),
            from: MetaProvenance::Instance,
            value: MetaValue {
                value: serde_json::json!("v"),
                guest_visible: true,
                guest_writable: false,
                updated_by: "test".to_string(),
                updated_at: chrono::Utc::now(),
            },
        }
    }

    /// Test source that counts how many times `get` was called, so
    /// the cache's hit/miss behaviour is directly observable.
    struct CountingSource {
        view: Vec<RealizedMetaEntry>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl RealizedDataSource for CountingSource {
        async fn get(&self, _: Uuid) -> Result<Vec<RealizedMetaEntry>, RealizedFetchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.view.clone())
        }
    }

    #[tokio::test]
    async fn first_get_fetches_second_is_cached() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = RealizedViewCache::new(CountingSource {
            view: vec![entry("config/ntp-servers")],
            calls: calls.clone(),
        });
        let id = Uuid::new_v4();
        let a = cache.get(id).await.unwrap();
        let b = cache.get(id).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "second call hit cache");
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn invalidate_busts_the_cached_view() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = RealizedViewCache::new(CountingSource {
            view: vec![],
            calls: calls.clone(),
        });
        let id = Uuid::new_v4();
        cache.get(id).await.unwrap();
        cache.invalidate(id).await;
        cache.get(id).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn ttl_expiry_triggers_refetch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = RealizedViewCache::with_ttl(
            CountingSource {
                view: vec![],
                calls: calls.clone(),
            },
            Duration::from_millis(10),
        );
        let id = Uuid::new_v4();
        cache.get(id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        cache.get(id).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn upstream_error_does_not_cache_failure() {
        struct Boom;
        #[async_trait]
        impl RealizedDataSource for Boom {
            async fn get(&self, _: Uuid) -> Result<Vec<RealizedMetaEntry>, RealizedFetchError> {
                Err(RealizedFetchError::Backend("boom".into()))
            }
        }
        let cache = RealizedViewCache::new(Boom);
        let id = Uuid::new_v4();
        assert!(cache.get(id).await.is_err());
        // A subsequent get tries again rather than returning a
        // cached failure.
        assert!(cache.get(id).await.is_err());
    }

    #[tokio::test]
    async fn clear_drops_everything() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = RealizedViewCache::new(CountingSource {
            view: vec![],
            calls: calls.clone(),
        });
        cache.get(Uuid::new_v4()).await.unwrap();
        cache.get(Uuid::new_v4()).await.unwrap();
        cache.clear().await;
        cache.get(Uuid::new_v4()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
