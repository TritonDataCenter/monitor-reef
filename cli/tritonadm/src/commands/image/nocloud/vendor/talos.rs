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
//! The factory does not publish per-image sha256/sha512 sidecars
//! (the HEAD requests for `.sha256` and `.sha512` come back 402),
//! and the `sha256sum.txt` in the upstream GitHub release covers
//! only the official metal/ISO assets, not factory-built nocloud
//! images. So the verifier is `TlsTrustOnly`: we explicitly note
//! the trust model rather than silently skipping.
//!
//! Talos rejects ssh-key injection via cloud-init (kubelet/etcd is
//! the only access path), so `ssh_key` is `false` in the manifest
//! requirements.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::TlsTrustOnly;

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

        let url: Url = format!(
            "https://factory.talos.dev/image/{DEFAULT_SCHEMATIC}/v{version}/nocloud-amd64.raw.xz"
        )
        .parse()
        .context("talos factory image url")?;

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
            verifier: Box::new(TlsTrustOnly {
                note: "Talos factory does not publish per-image hashes".to_string(),
            }),
            expected_sha256: None,
        })
    }
}
