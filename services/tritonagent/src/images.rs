// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image content fetcher.
//!
//! Replaces `imgadm import` with a direct ZFS path: download
//! the gzipped `zfs send` stream from `image.source_url`,
//! sha256-verify against `image.sha256`, then `gzip -dc | zfs
//! receive zones/<image_id>` and snapshot as `@final` so
//! `vmadm create` can clone from it.
//!
//! ## Idempotency
//!
//! Returns `Ok(())` immediately when `zones/<image_id>@final`
//! already exists on this host. Subsequent provisions of the
//! same image skip the download and the receive — only the
//! first instance using a given image pays the import cost.
//!
//! ## sha256 is a tamper boundary
//!
//! A mismatch between `image.sha256` and the downloaded
//! content is reported as an unrecoverable failure
//! (`JobOutcome::Failed { reason }`); we do **not** install
//! the dataset. This is the only check that prevents an
//! attacker who controls `source_url` from substituting
//! arbitrary content under tritond's image identity.
//!
//! ## Cache layout
//!
//! Downloaded `.gz` files land under `/var/tmp/triton-images/`.
//! After a successful `zfs receive` they are deleted — the
//! ZFS dataset *is* the cache. A future slice can keep the
//! `.gz` around for offline re-imports if needed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::info;
use tritond_client::types::Image;

use crate::zfs;

/// Where downloaded `.gz` content lands intermediate to the
/// `zfs receive`. One `.gz` per image-id, removed after a
/// successful import.
const CACHE_DIR: &str = "/var/tmp/triton-images";

/// Ensure the image's content is materialised on this host as
/// `zones/<image_id>@final`. Idempotent — returns immediately
/// when the dataset already exists.
pub async fn ensure(image: &Image) -> Result<()> {
    let dataset = format!("zones/{}", image.id);
    let snap_full = format!("{dataset}@final");
    if zfs::snapshot_exists(&snap_full)
        .await
        .context("zfs snapshot existence check")?
    {
        info!(image_id = %image.id, "image dataset already present; skipping fetch");
        return Ok(());
    }

    let source = image
        .source_url
        .as_deref()
        .ok_or_else(|| anyhow!("image {} has no source_url; cannot fetch content", image.id))?;

    fs::create_dir_all(CACHE_DIR)
        .await
        .with_context(|| format!("create cache dir {CACHE_DIR}"))?;
    let download_path: PathBuf = format!("{CACHE_DIR}/{}.gz", image.id).into();

    info!(image_id = %image.id, %source, "downloading image content");
    download_and_verify(source, &image.sha256, &download_path).await?;

    info!(image_id = %image.id, %dataset, "running gzip -dc | zfs receive");
    let recv_result = zfs::recv_gzipped(&dataset, &download_path).await;

    // Clean up the intermediate download regardless of recv
    // outcome. The .gz file is only useful as input to recv.
    let _ = fs::remove_file(&download_path).await;

    if let Err(e) = recv_result {
        // Best-effort: try to clean up a partial dataset so a
        // retry can `zfs receive` cleanly.
        let _ = zfs::destroy(&dataset).await;
        return Err(e).context("zfs receive of image content");
    }

    zfs::snapshot(&dataset, "final")
        .await
        .context("snapshot the freshly-received image dataset")?;
    info!(image_id = %image.id, "image installed");
    Ok(())
}

async fn download_and_verify(url: &str, expected_sha256: &str, dest: &Path) -> Result<()> {
    let resp = reqwest::get(url)
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error from {url}"))?;
    let mut stream = resp.bytes_stream();

    let mut file = fs::File::create(dest)
        .await
        .with_context(|| format!("create cache file {dest:?}"))?;
    let mut hasher = Sha256::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read response body chunk")?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .context("write content chunk to cache file")?;
    }
    file.flush().await.context("flush cache file")?;
    drop(file);

    let actual = format!("{:x}", hasher.finalize());
    let expected = expected_sha256.to_ascii_lowercase();
    if actual != expected {
        // Refuse to install. Cleanup happens at the call site
        // when this error bubbles up.
        let _ = fs::remove_file(dest).await;
        bail!("image content sha256 mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}
