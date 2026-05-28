// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! AlmaLinux cloud-image vendor profile.
//!
//! AlmaLinux publishes GenericCloud qcow2 images per major at
//! `https://repo.almalinux.org/almalinux/<major>/cloud/x86_64/images/`.
//! The directory listing at `/almalinux/` enumerates supported
//! majors (8, 9, 10, …); the `-latest.x86_64.qcow2` filename is a
//! rolling pointer to whatever build is current. The sibling
//! `CHECKSUM` file is Linux-style (`<sha256>  <filename>`), and
//! since the latest pointer and its dated alias share a hash we
//! can resolve the dated form once at metadata time and verify
//! with a plain `Sha256Pinned`.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{PinnedQcow2, ResolvedImage, VendorProfile};

pub struct Alma;

#[async_trait]
impl VendorProfile for Alma {
    fn name(&self) -> &str {
        "alma"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("alma image url")?;
        PinnedQcow2 {
            url,
            series: format!("alma{}", resolved.major),
            // Build identifier (e.g. `9.7-20260501`) so distinct
            // rebuilds dedupe in the manifest.
            version: resolved.build,
            description: format!(
                "AlmaLinux {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.major
            ),
            homepage: "https://almalinux.org/",
            sha256: resolved.sha256,
        }
        .into_resolved("alma")
    }
}
