// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! openSUSE Leap cloud-image vendor profile.
//!
//! Leap publishes per-version Minimal-VM Cloud qcow2 images at
//! `https://download.opensuse.org/distribution/leap/<X>.<Y>/appliances/`.
//! Release discovery uses MirrorCache's JSON listing
//! (`?json=1`); the sidecar is a Linux-style `.sha256` sibling so
//! we pin the hash at metadata time and `--dry-run` shows the
//! manifest UUID. Tumbleweed is intentionally skipped — its
//! current `appliances/` directory only ships MicroOS-flavored
//! immutable images (Combustion/Ignition, not cloud-init nocloud).

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256Pinned;

pub struct OpenSuse;

#[async_trait]
impl VendorProfile for OpenSuse {
    fn name(&self) -> &str {
        "opensuse"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("opensuse image url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            series: format!("leap{}", resolved.leap_version),
            version: resolved.build.clone(),
            description: format!(
                "openSUSE Leap {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.leap_version
            ),
            homepage: Url::parse("https://www.opensuse.org/").context("opensuse homepage url")?,
            ssh_key: true,
            verifier: Box::new(Sha256Pinned(resolved.sha256.clone())),
            expected_sha256: Some(resolved.sha256),
        })
    }
}
