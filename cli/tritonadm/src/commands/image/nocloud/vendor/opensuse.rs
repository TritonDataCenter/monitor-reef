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

use super::{PinnedQcow2, ResolvedImage, VendorProfile};

pub struct OpenSuse;

#[async_trait]
impl VendorProfile for OpenSuse {
    fn name(&self) -> &str {
        "opensuse"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("opensuse image url")?;
        PinnedQcow2 {
            url,
            series: format!("leap{}", resolved.leap_version),
            version: resolved.build,
            description: format!(
                "openSUSE Leap {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.leap_version
            ),
            homepage: "https://www.opensuse.org/",
            sha256: resolved.sha256,
        }
        .into_resolved("opensuse")
    }
}
