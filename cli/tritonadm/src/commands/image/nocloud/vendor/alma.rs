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

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256Pinned;

pub struct Alma;

#[async_trait]
impl VendorProfile for Alma {
    fn name(&self) -> &str {
        "alma"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("alma image url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            series: format!("alma{}", resolved.major),
            // Build identifier (e.g. `9.7-20260501`) so distinct
            // rebuilds dedupe in the manifest.
            version: resolved.build.clone(),
            description: format!(
                "AlmaLinux {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.major
            ),
            homepage: Url::parse("https://almalinux.org/").context("alma homepage url")?,
            ssh_key: true,
            verifier: Box::new(Sha256Pinned(resolved.sha256.clone())),
            expected_sha256: Some(resolved.sha256),
        })
    }
}
