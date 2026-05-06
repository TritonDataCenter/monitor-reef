// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Rocky Linux cloud-image vendor profile.
//!
//! Rocky publishes GenericCloud-Base qcow2 images per major at
//! `https://download.rockylinux.org/pub/rocky/<major>/images/x86_64/`,
//! with a per-file BSD-style `.CHECKSUM` sidecar
//! (`SHA256 (filename) = hex`). The release-resolution path fetches
//! both the directory listing and the chosen build's sidecar, so the
//! upstream sha256 is already known by the time the verifier runs —
//! we use a plain `Sha256Pinned` and `--dry-run` can show the
//! derived manifest UUID without downloading the qcow2.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256Pinned;

pub struct Rocky;

#[async_trait]
impl VendorProfile for Rocky {
    fn name(&self) -> &str {
        "rocky"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("rocky image url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            series: format!("rocky{}", resolved.major),
            version: resolved.build.clone(),
            description: format!(
                "Rocky Linux {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.major
            ),
            homepage: Url::parse("https://rockylinux.org/").context("rocky homepage url")?,
            ssh_key: true,
            verifier: Box::new(Sha256Pinned(resolved.sha256.clone())),
            expected_sha256: Some(resolved.sha256),
        })
    }
}
