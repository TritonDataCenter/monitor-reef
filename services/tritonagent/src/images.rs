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

/// Build a reqwest client suitable for fetching image content.
///
/// We don't use `reqwest::get` (which constructs a default
/// client) because on a cold SmartOS GZ that path tries to
/// initialise the platform CA verifier and panics — even when
/// the URL is plaintext HTTP, reqwest still arms TLS for
/// potential redirects. Mirrors the pre-configured webpki-roots
/// trick from [`crate::AgentConfig::build_client`] but without
/// the bearer auth headers (we don't want to send tritond's
/// API key to a third-party image source).
fn build_fetch_client() -> Result<reqwest::Client> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .build()
        .context("build fetch reqwest client")
}

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
        // The dataset exists, but the imgadm-side manifest may
        // not — for example, on a first run this branch fires
        // because we created the snapshot but bombed before
        // publishing the manifest. Always publish so vmadm can
        // resolve `image_uuid` regardless of how we got here.
        write_imgadm_manifest(image)
            .await
            .context("publish synthetic manifest into /var/imgadm/images")?;
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

    // Two on-disk shapes for `source_url`:
    //
    //   * legacy bare-gz: pre-slice-B images (no compatibility
    //     metadata) point at a gzipped `zfs send` stream
    //     directly. We download it, sha256-verify, and feed
    //     it to `zfs receive`.
    //
    //   * tritond bundle: slice-B images (compatibility set)
    //     point at a tar bundle produced by `tritonimg-build`.
    //     We download the tar, extract `content.zfs.gz` to
    //     `download_path`, sha256-verify the extracted bytes,
    //     and feed the same `zfs receive` path. The manifest
    //     was already validated server-side at ingest, so we
    //     don't re-parse it here.
    info!(image_id = %image.id, %source, "downloading image content");
    if image.compatibility.is_some() {
        download_bundle_extract_content(source, &image.sha256, &download_path).await?;
    } else {
        download_and_verify(source, &image.sha256, &download_path).await?;
    }

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

    // `zfs send <ds>@<snap>` produces a stream that includes the
    // source snapshot, so `zfs receive` typically recreates
    // `<dataset>@final` for us. Only snapshot here if the
    // received stream didn't already carry one — handles both
    // the standard imgadm-style send and the rare snapshot-less
    // input shape.
    if !zfs::snapshot_exists(&snap_full).await? {
        zfs::snapshot(&dataset, "final")
            .await
            .context("snapshot the freshly-received image dataset")?;
    }

    // vmadm validates `image_uuid` against imgadm's per-host
    // catalog (the JSON files under /var/imgadm/images/) before
    // it'll clone the dataset. Drop a minimal manifest in there
    // so vmadm "sees" our agent-installed image without the
    // operator ever running `imgadm import`. The manifest
    // satisfies vmadm's metadata reads (type, os, requirements);
    // imgadm doesn't re-validate the file sha at create time so
    // the placeholder sha1 is harmless. A future slice replaces
    // this with the tritond bundle format and a manifest that
    // also carries `min_smartos_platform` enforcement.
    write_imgadm_manifest(image)
        .await
        .context("publish synthetic manifest into /var/imgadm/images")?;

    info!(image_id = %image.id, "image installed");
    Ok(())
}

async fn write_imgadm_manifest(image: &Image) -> Result<()> {
    fs::create_dir_all("/var/imgadm/images")
        .await
        .context("create /var/imgadm/images")?;
    let path = format!("/var/imgadm/images/zones-{}.json", image.id);
    // Format published_at as a UTC timestamp — imgadm requires
    // an ISO-8601 instant. Use the image record's created_at if
    // present-day formatting is preferred; for v0 just stamp
    // "now" — it's only used by `imgadm list` ordering.
    let published_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let manifest = serde_json::json!({
        "manifest": {
            "v": 2,
            "uuid": image.id,
            "owner": "00000000-0000-0000-0000-000000000000",
            "name": image.name,
            "version": image.version,
            "state": "active",
            "disabled": false,
            "public": false,
            "published_at": published_at,
            "type": "zone-dataset",
            "os": image.os,
            "files": [{
                // imgadm validates this only at install time
                // (which we bypassed by doing zfs receive
                // directly). Placeholder is fine.
                "sha1": "0000000000000000000000000000000000000000",
                "size": image.size_bytes,
                "compression": "gzip"
            }],
            "description": image.description,
            "requirements": {}
        },
        "zpool": "zones",
        "source": "tritond"
    });
    let body =
        serde_json::to_vec_pretty(&manifest).context("serialise synthetic imgadm manifest")?;
    fs::write(&path, &body)
        .await
        .with_context(|| format!("write {path}"))?;
    Ok(())
}

/// Download a tritond image bundle from `url`, extract
/// `content.zfs.gz` into `content_dest`, and sha256-verify the
/// extracted bytes against `expected_sha256` (which must equal
/// the manifest's `content.sha256` — the agent doesn't trust
/// the manifest on disk, only the value tritond gave us via
/// the Image record). The intermediate tar is unpacked to a
/// sibling temp directory and removed on the way out.
async fn download_bundle_extract_content(
    url: &str,
    expected_sha256: &str,
    content_dest: &Path,
) -> Result<()> {
    let parent = content_dest
        .parent()
        .ok_or_else(|| anyhow!("bundle content dest has no parent dir: {content_dest:?}"))?;
    let bundle_path = parent.join(format!(
        "{}.tar",
        content_dest
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("bundle")
    ));
    download_to_path(url, &bundle_path).await?;

    // Extract to a per-call temp dir so concurrent imports of
    // distinct images don't clobber each other's content.zfs.gz.
    let extract_dir = parent.join(format!(
        "extract-{}",
        content_dest
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("bundle")
    ));
    let _ = fs::remove_dir_all(&extract_dir).await;
    let extract_dir_clone = extract_dir.clone();
    let bundle_for_blocking = bundle_path.clone();
    let extracted = tokio::task::spawn_blocking(move || {
        tritond_image_manifest::extract_bundle(&bundle_for_blocking, &extract_dir_clone)
    })
    .await
    .context("join extract task")?
    .context("extract bundle tar")?;
    let _ = fs::remove_file(&bundle_path).await;

    // Move the extracted content into the canonical
    // `download_path` slot so the rest of the install path
    // (zfs receive) doesn't care which branch we took.
    fs::rename(&extracted.content_path, content_dest)
        .await
        .with_context(|| {
            format!(
                "move extracted content {:?} → {:?}",
                extracted.content_path, content_dest,
            )
        })?;
    let _ = fs::remove_dir_all(&extract_dir).await;

    // Re-verify against the operator-provided sha256 (which
    // tritond stored on the Image record at ingest time and
    // already cross-checked against the manifest). A second
    // check here hardens against a compromised on-disk cache
    // or a torn rename.
    verify_sha256(content_dest, expected_sha256).await
}

async fn download_to_path(url: &str, dest: &Path) -> Result<()> {
    let client = build_fetch_client()?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error from {url}"))?;
    let mut stream = resp.bytes_stream();
    let mut file = fs::File::create(dest)
        .await
        .with_context(|| format!("create {dest:?}"))?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read body chunk")?;
        file.write_all(&chunk)
            .await
            .context("write chunk to file")?;
    }
    file.flush().await.context("flush download")?;
    Ok(())
}

async fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = fs::File::open(path)
        .await
        .with_context(|| format!("open {path:?} for hashing"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    use tokio::io::AsyncReadExt as _;
    loop {
        let n = file.read(&mut buf).await.context("read for hashing")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = format!("{:x}", hasher.finalize());
    let want = expected.to_ascii_lowercase();
    if actual != want {
        let _ = fs::remove_file(path).await;
        bail!("image content sha256 mismatch: expected {want}, got {actual}");
    }
    Ok(())
}

async fn download_and_verify(url: &str, expected_sha256: &str, dest: &Path) -> Result<()> {
    let client = build_fetch_client()?;
    let resp = client
        .get(url)
        .send()
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
