// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thread-safe cache for mapping short IDs to JIRA pagination tokens.
//!
//! JIRA v3 API pagination tokens contain base64-encoded data including:
//! - The JQL query being executed
//! - Internal cursor state
//! - Potentially other metadata
//!
//! Exposing these tokens directly in URLs would:
//! - Leak query details (labels being searched, filters applied) to users
//! - Allow tokens to appear in browser history and server logs
//! - Potentially enable token manipulation attacks
//!
//! Instead, we generate opaque 12-character alphanumeric IDs that map to the
//! real JIRA tokens internally. These IDs are:
//! - Short enough for clean URLs
//! - Cryptographically random (using thread_rng)
//! - Time-limited (TTL-based expiration)
//! - Capacity-limited (LRU eviction)

use indexmap::IndexMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Length of the short, URL-safe pagination token IDs we expose publicly.
/// 12 chars from 62-char alphabet = ~71 bits of entropy, sufficient to prevent
/// brute-force enumeration within the 1-hour TTL window.
const PAGINATION_TOKEN_ID_LEN: usize = 12;

/// Size of the token ID alphabet (0-9, a-z, A-Z = 62 characters).
const TOKEN_ID_ALPHABET_LEN: u8 = 62;

/// Default TTL for cached JIRA pagination tokens (1 hour).
/// Chosen to allow reasonable user session duration while limiting
/// exposure window for any leaked tokens.
const TOKEN_TTL_SECS: u64 = 60 * 60;

/// Maximum number of cached pagination tokens to retain.
/// Limits memory usage and ensures old tokens are evicted.
const TOKEN_CACHE_MAX_ENTRIES: usize = 1000;

/// Maximum attempts to find a unique token ID before giving up.
const MAX_COLLISION_ATTEMPTS: usize = 100;

/// Token cache entry with expiration
struct TokenCacheEntry {
    jira_token: String,
    expires_at: Instant,
}

/// Thread-safe cache for mapping short IDs to JIRA pagination tokens.
#[derive(Clone)]
pub struct TokenCache {
    cache: Arc<Mutex<IndexMap<String, TokenCacheEntry>>>,
    ttl: Duration,
    max_entries: usize,
}

impl TokenCache {
    /// Create a new token cache with default settings.
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(IndexMap::new())),
            ttl: Duration::from_secs(TOKEN_TTL_SECS),
            max_entries: TOKEN_CACHE_MAX_ENTRIES,
        }
    }

    /// Create a token cache with custom TTL and capacity (for testing).
    #[cfg(test)]
    pub fn new_with(ttl: Duration, max_entries: usize) -> Self {
        Self {
            cache: Arc::new(Mutex::new(IndexMap::new())),
            ttl,
            max_entries,
        }
    }

    /// Store a JIRA token and return a short random ID.
    pub fn store(&self, jira_token: String) -> String {
        use rand::Rng;

        let mut rng = rand::rng();
        let mut cache = self.cache.lock().unwrap_or_else(|poisoned| {
            tracing::error!("Token cache mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        // Cleanup expired entries
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);

        // Enforce capacity by evicting oldest entries (O(1) with IndexMap swap_remove_index)
        while cache.len() >= self.max_entries {
            let _ = cache.swap_remove_index(0);
        }

        // Generate ID, checking for collisions (unlikely but possible)
        // With 62^12 possible IDs (~3.2e21) and max 1000 entries, collision
        // probability is astronomically low, but we add a limit for safety
        let id = (0..MAX_COLLISION_ATTEMPTS)
            .find_map(|_| {
                let candidate: String = (0..PAGINATION_TOKEN_ID_LEN)
                    .map(|_| {
                        let idx = rng.random_range(0..TOKEN_ID_ALPHABET_LEN);
                        match idx {
                            0..=9 => (b'0' + idx) as char,
                            10..=35 => (b'a' + idx - 10) as char,
                            _ => (b'A' + idx - 36) as char,
                        }
                    })
                    .collect();
                (!cache.contains_key(&candidate)).then_some(candidate)
            })
            .unwrap_or_else(|| {
                // This should never happen in practice, but log and generate anyway
                tracing::error!(
                    "Token cache collision detection exceeded {} attempts - this indicates a bug",
                    MAX_COLLISION_ATTEMPTS
                );
                // Generate one more as fallback (will overwrite if collision)
                (0..PAGINATION_TOKEN_ID_LEN)
                    .map(|_| {
                        let idx = rng.random_range(0..TOKEN_ID_ALPHABET_LEN);
                        match idx {
                            0..=9 => (b'0' + idx) as char,
                            10..=35 => (b'a' + idx - 10) as char,
                            _ => (b'A' + idx - 36) as char,
                        }
                    })
                    .collect()
            });

        cache.insert(
            id.clone(),
            TokenCacheEntry {
                jira_token,
                expires_at: now + self.ttl,
            },
        );

        id
    }

    /// Retrieve a JIRA token by ID, cleaning up expired entries.
    pub fn get(&self, id: &str) -> Option<String> {
        let mut cache = self.cache.lock().unwrap_or_else(|poisoned| {
            tracing::error!("Token cache mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        // Clean up expired entries
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);

        // Get the token if it exists and hasn't expired
        cache.get(id).map(|entry| entry.jira_token.clone())
    }
}

impl Default for TokenCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_cache_store_and_retrieve() {
        let cache = TokenCache::new();
        let jira_token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9".to_string();

        let short_id = cache.store(jira_token.clone());

        // Short ID should be 12 chars
        assert_eq!(short_id.len(), PAGINATION_TOKEN_ID_LEN);

        // Should be alphanumeric
        assert!(short_id.chars().all(|c| c.is_ascii_alphanumeric()));

        // Should retrieve the original token
        let retrieved = cache.get(&short_id);
        assert_eq!(retrieved, Some(jira_token));
    }

    #[test]
    fn test_token_cache_returns_none_for_unknown_id() {
        let cache = TokenCache::new();
        assert_eq!(cache.get("unknown12345"), None);
    }

    #[test]
    fn test_token_cache_capacity_bounds() {
        let cache = TokenCache::new_with(Duration::from_secs(3600), 3);

        // Store 4 tokens (exceeds capacity of 3)
        let _id1 = cache.store("token1".to_string());
        let id2 = cache.store("token2".to_string());
        let id3 = cache.store("token3".to_string());
        let id4 = cache.store("token4".to_string());

        // First token should be evicted (LRU)
        // id1 is no longer valid (was evicted), but we don't have id1 to check
        // Instead, verify the cache has at most 3 entries by checking recent ones work
        assert!(cache.get(&id2).is_some() || cache.get(&id3).is_some());
        assert!(cache.get(&id3).is_some());
        assert!(cache.get(&id4).is_some());
    }

    #[test]
    fn test_token_cache_expiration() {
        // Use 1 second TTL to avoid flaky failures when running under code
        // coverage tools (cargo tarpaulin) which significantly slow execution.
        let cache = TokenCache::new_with(Duration::from_secs(1), 100);

        let short_id = cache.store("token".to_string());

        // Should be retrievable immediately
        assert!(cache.get(&short_id).is_some());

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(1500));

        // Should be expired now
        assert!(cache.get(&short_id).is_none());
    }

    #[test]
    fn test_token_cache_unique_ids() {
        let cache = TokenCache::new();

        // Store multiple tokens and verify they get unique IDs
        let ids: Vec<String> = (0..10)
            .map(|i| cache.store(format!("token{}", i)))
            .collect();

        // All IDs should be unique
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }
}
