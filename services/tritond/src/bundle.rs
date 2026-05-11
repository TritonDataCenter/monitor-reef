// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image-bundle ingest: fetch a tritond image bundle, verify its
//! content hash against the manifest, and produce a [`NewImage`].

use anyhow::Context;

use tritond_api::types::{ImageCompatibility, NewImage};

/// Fetch a tritond image bundle from `bundle_url`, parse the
/// manifest, re-hash the content against the manifest's claimed
/// sha256, and return a [`NewImage`] populated from the
/// manifest. The bundle URL is recorded as the resulting Image's
/// `source_url` so the per-CN agent can fetch the same bundle
/// at provision time.
///
/// All manifest fields ride into the Image record verbatim
/// (name, version, sha256, size, compatibility, os_family).
/// `description` falls back to empty when the manifest doesn't
/// carry one.
///
/// The downloaded bundle is extracted to a `tempfile::TempDir`
/// that drops at function exit — tritond doesn't cache the
/// content, the agent re-downloads on first provision per CN.
pub(crate) async fn ingest_bundle(bundle_url: &str) -> anyhow::Result<NewImage> {
    use sha2::{Digest, Sha256};

    // Pre-configured TLS using webpki-roots. Same reason as the
    // agent: cold SmartOS GZ has no platform CA store.
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .build()
        .context("build bundle-fetch reqwest client")?;

    let work = tempfile::tempdir().context("create temp dir for bundle ingest")?;
    let bundle_path = work.path().join("bundle.tar");

    // Stream the bundle to disk so very large bundles don't
    // need to fit in memory.
    let bytes = client
        .get(bundle_url)
        .send()
        .await
        .with_context(|| format!("GET {bundle_url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error from {bundle_url}"))?
        .bytes()
        .await
        .with_context(|| format!("read bundle body from {bundle_url}"))?;
    // Phase 0 reads the entire bundle into memory before
    // writing — bundles for OS images are typically tens of MB
    // gzipped, well within tritond's RAM budget. A future slice
    // adds streaming when bundles routinely exceed ~1 GB.
    tokio::fs::write(&bundle_path, &bytes)
        .await
        .context("persist bundle to temp file")?;

    let extracted = tritond_image_manifest::extract_bundle(&bundle_path, work.path())
        .context("extract bundle tar")?;

    // Re-hash the content. The manifest's sha256 is operator-
    // provided (via the build CLI); we don't trust it without
    // verification, otherwise an attacker who controls the
    // bundle URL could substitute arbitrary content under any
    // claimed hash.
    let mut hasher = Sha256::new();
    let mut content_file = tokio::fs::File::open(&extracted.content_path)
        .await
        .context("open extracted content for hashing")?;
    use tokio::io::AsyncReadExt as _;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = content_file
            .read(&mut buf)
            .await
            .context("read extracted content")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if actual_sha256 != extracted.manifest.content.sha256.to_ascii_lowercase() {
        anyhow::bail!(
            "bundle content sha256 mismatch: manifest claims {}, actual {actual_sha256}",
            extracted.manifest.content.sha256,
        );
    }
    if total != extracted.manifest.content.size {
        // Defensive — a length mismatch on a hash-matching
        // payload is impossible barring a sha256 collision,
        // but we surface it for diagnosability.
        anyhow::bail!(
            "bundle content size mismatch: manifest claims {}, actual {total}",
            extracted.manifest.content.size,
        );
    }

    Ok(NewImage {
        name: extracted.manifest.name,
        description: extracted.manifest.description,
        os: extracted.manifest.guest.os_family,
        version: extracted.manifest.version,
        size_bytes: extracted.manifest.content.size,
        sha256: extracted.manifest.content.sha256,
        source_url: Some(bundle_url.to_string()),
        id: None,
        compatibility: Some(ImageCompatibility {
            brand: extracted.manifest.compatibility.brand,
            arch: extracted.manifest.compatibility.arch,
            min_smartos_platform: extracted.manifest.compatibility.min_smartos_platform,
        }),
    })
}
