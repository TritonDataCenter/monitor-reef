// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Automatic pagination for list endpoints.
//!
//! Node.js triton uses LOMStream to automatically page through results in
//! 1000-item chunks (limit+offset) until the server returns fewer items
//! than the page size. This module provides the equivalent for Rust.

use std::future::Future;

/// Default page size matching node-triton's LOMStream default.
pub const DEFAULT_PAGE_SIZE: u64 = 1000;

/// Fetch all pages from a paginated list endpoint.
///
/// Calls `fetch_page(limit, offset)` repeatedly, accumulating results,
/// until the server returns fewer items than `page_size` (indicating the
/// last page) or `max_results` is reached.
///
/// # Arguments
/// * `page_size` - Number of items per page (typically [`DEFAULT_PAGE_SIZE`])
/// * `max_results` - Optional cap on total items returned. When set, the last
///   page request uses a reduced limit to avoid over-fetching.
/// * `fetch_page` - Closure that takes `(limit, offset)` and returns a future
///   resolving to a `Vec<T>` or error.
pub async fn paginate_all<T, E, F, Fut>(
    page_size: u64,
    max_results: Option<u64>,
    fetch_page: F,
) -> Result<Vec<T>, E>
where
    F: Fn(u64, u64) -> Fut,
    Fut: Future<Output = Result<Vec<T>, E>>,
{
    let mut all_results: Vec<T> = Vec::new();
    let mut offset: u64 = 0;

    loop {
        // When max_results is set, reduce the page limit on the last
        // request so we don't fetch more items than needed.
        let remaining = max_results.map(|max| max.saturating_sub(offset));
        let limit = match remaining {
            Some(0) => break,
            Some(r) => r.min(page_size),
            None => page_size,
        };

        let page = fetch_page(limit, offset).await?;
        let page_len = page.len() as u64;
        all_results.extend(page);
        offset += page_len;

        // Last page: server returned fewer items than requested
        if page_len < limit {
            break;
        }

        // Reached the cap
        if let Some(max) = max_results
            && offset >= max
        {
            all_results.truncate(max as usize);
            break;
        }
    }

    Ok(all_results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn single_page() {
        let items: Vec<Vec<u32>> = vec![vec![1, 2, 3]];
        let items_clone = items.clone();
        let result = paginate_all(10, None, |_limit, _offset| {
            let items = items_clone.clone();
            async move { Ok::<_, String>(items[0].clone()) }
        })
        .await
        .unwrap();
        assert_eq!(result, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn multi_page() {
        let pages: Vec<Vec<u32>> = vec![vec![1, 2], vec![3, 4], vec![5]];
        let pages_clone = pages.clone();
        let result = paginate_all(2, None, |_limit, offset| {
            let pages = pages_clone.clone();
            async move {
                let page_idx = (offset / 2) as usize;
                Ok::<_, String>(pages.get(page_idx).cloned().unwrap_or_default())
            }
        })
        .await
        .unwrap();
        assert_eq!(result, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn max_results_cap() {
        // 3 full pages available, but cap at 5
        let result = paginate_all(3, Some(5), |_limit, offset| async move {
            let start = offset as u32;
            Ok::<_, String>(vec![start + 1, start + 2, start + 3])
        })
        .await
        .unwrap();
        assert_eq!(result, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn max_results_reduces_last_limit() {
        // Cap at 4 items, page size 3: second request should use limit=1
        let limits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let limits_clone = limits.clone();
        let result = paginate_all(3, Some(4), |limit, offset| {
            let limits = limits_clone.clone();
            async move {
                limits.lock().unwrap().push(limit);
                let count = limit as usize;
                let start = offset as u32;
                Ok::<_, String>((1..=count as u32).map(|i| start + i).collect())
            }
        })
        .await
        .unwrap();
        let limits_seen = limits.lock().unwrap().clone();
        assert_eq!(result, vec![1, 2, 3, 4]);
        assert_eq!(limits_seen, vec![3, 1]);
    }

    #[tokio::test]
    async fn empty_response() {
        let result = paginate_all(10, None, |_limit, _offset| async move {
            Ok::<_, String>(Vec::<u32>::new())
        })
        .await
        .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn error_propagation() {
        let result: Result<Vec<u32>, String> =
            paginate_all(10, None, |_limit, _offset| async move {
                Err("test error".to_string())
            })
            .await;
        assert_eq!(result.unwrap_err(), "test error");
    }

    #[tokio::test]
    async fn max_results_zero() {
        let result = paginate_all(10, Some(0), |_limit, _offset| async move {
            panic!("should not be called");
            #[allow(unreachable_code)]
            Ok::<_, String>(Vec::<u32>::new())
        })
        .await
        .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn exact_page_boundary() {
        // When the last page is exactly full, we make one more request
        // that returns empty
        let pages: Vec<Vec<u32>> = vec![vec![1, 2], vec![3, 4], vec![]];
        let pages_clone = pages.clone();
        let result = paginate_all(2, None, |_limit, offset| {
            let pages = pages_clone.clone();
            async move {
                let page_idx = (offset / 2) as usize;
                Ok::<_, String>(pages.get(page_idx).cloned().unwrap_or_default())
            }
        })
        .await
        .unwrap();
        assert_eq!(result, vec![1, 2, 3, 4]);
    }
}
