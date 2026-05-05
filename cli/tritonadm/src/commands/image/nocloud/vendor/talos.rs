// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Talos Linux cloud-image vendor profile.
//!
//! Talos publishes nocloud-flavored bhyve-suitable images via its
//! Image Factory at `https://factory.talos.dev/image/<schematic>/v<version>/nocloud-amd64.raw.xz`.
//! Each "schematic" is a content-addressed (sha256) bundle of
//! customizations; the value baked in here is the canonical empty
//! schematic — vanilla Talos with no extensions, matching the bash
//! builder this profile replaces.
//!
//! The factory's API documents `.sha256` / `.sha512` checksum
//! endpoints, but Sidero gates them behind an enterprise licence on
//! the public `factory.talos.dev` (free-tier requests return
//! `HTTP 402 "enterprise not enabled"`). Self-hosted or
//! enterprise factories return the checksum normally. We probe the
//! endpoint at resolve time: if it responds with a checksum, we use
//! `Sha256SumsTls` against it; if it 402s, we fall back to
//! `TlsTrustOnly` and explicitly note the trust model rather than
//! silently skipping. Either way the operator can pin a hash via
//! `--expected-sha256 <hex>`.
//!
//! Talos rejects ssh-key injection via cloud-init (kubelet/etcd is
//! the only access path), so `ssh_key` is `false` in the manifest
//! requirements.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::{Sha256SumsTls, TlsTrustOnly, Verifier};

pub struct Talos;

/// The Talos default (empty) schematic. Schematic IDs are
/// sha256(customizations_yaml); this is the well-known sha for "no
/// customizations." Stable forever.
const DEFAULT_SCHEMATIC: &str =
    "376567988ad370138ad8b2698212367b8edcb69b5fd68c80be1f2ec7d603b4ba";

#[async_trait]
impl VendorProfile for Talos {
    fn name(&self) -> &str {
        "talos"
    }

    async fn resolve(
        &self,
        release: &str,
        http: &reqwest::Client,
    ) -> Result<ResolvedImage> {
        let version = if release.trim() == "latest" {
            releases::find_latest(http).await?
        } else {
            releases::parse_version(release)?
        };

        // Major.minor as series (e.g. "1.13") so different patch
        // versions of the same minor share a manifest name.
        let series = match version.rsplit_once('.') {
            Some((prefix, _)) => prefix.to_string(),
            None => version.clone(),
        };

        let filename = "nocloud-amd64.raw.xz";
        let url: Url = format!(
            "https://factory.talos.dev/image/{DEFAULT_SCHEMATIC}/v{version}/{filename}"
        )
        .parse()
        .context("talos factory image url")?;
        let sums_url: Url = format!("{url}.sha256")
            .parse()
            .context("talos factory sha256 sidecar url")?;

        let verifier = pick_verifier(http, sums_url, filename.to_string()).await;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Xz,
            os: "linux".to_string(),
            series,
            version: version.clone(),
            description: format!(
                "Talos Linux v{version} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines."
            ),
            homepage: Url::parse("https://www.talos.dev/")
                .context("talos homepage url")?,
            // Talos's API surface for in-image management is via
            // kubelet/etcd; cloud-init ssh-key injection is rejected.
            ssh_key: false,
            verifier,
            expected_sha256: None,
        })
    }
}

/// Probe the factory's `.sha256` endpoint. Self-hosted / enterprise
/// factories return `200 <hex>  <filename>`; the public free-tier
/// `factory.talos.dev` returns `402 enterprise not enabled`. On
/// success we use `Sha256SumsTls`; on 402 we fall back to
/// `TlsTrustOnly` with a note. The probe is a HEAD-equivalent GET
/// of a small resource — single roundtrip, body discarded.
async fn pick_verifier(
    http: &reqwest::Client,
    sums_url: Url,
    filename: String,
) -> Box<dyn Verifier> {
    match http.get(sums_url.clone()).send().await {
        Ok(resp) if resp.status().is_success() => {
            Box::new(Sha256SumsTls::new(sums_url, filename))
        }
        _ => Box::new(TlsTrustOnly {
            note: "Talos public factory does not publish per-image hashes \
                   (enterprise feature). Pass --expected-sha256 to pin a hash."
                .to_string(),
        }),
    }
}
