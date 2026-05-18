/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

use anyhow::Result;
use std::future::Future;
use std::time::Duration;

const MAX_ATTEMPTS: u32 = 60;
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Run an async operation with exponential backoff retry.
///
/// The operation is retried up to `MAX_ATTEMPTS` times with exponential
/// backoff starting at 1s and capping at 30s.
pub async fn with_retry<F, Fut, T>(verbose: bool, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 0u32;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        attempt += 1;
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                if attempt >= MAX_ATTEMPTS {
                    return Err(e.context(format!("failed after {} attempts", MAX_ATTEMPTS)));
                }

                if verbose {
                    eprintln!(
                        "attempt {}/{} failed: {}; retrying in {:?}",
                        attempt, MAX_ATTEMPTS, e, backoff
                    );
                } else {
                    eprintln!(
                        "retrying in {:?} ({}/{})...",
                        backoff, attempt, MAX_ATTEMPTS
                    );
                }

                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
            }
        }
    }
}
