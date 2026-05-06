// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SmartOS cloud-image vendor profile.
//!
//! SmartOS publishes per-release VMware VM tarballs at
//! `https://us-central.manta.mnx.io/Joyent_Dev/public/SmartOS/<release>/`,
//! with a sibling `latest` text file pointing at the current release
//! id. Each tarball contains a `SmartOS.vmwarevm/` directory with a
//! monolithicFlat `SmartOS.vmdk` descriptor + `smartos.img` extent
//! (and a few VMware-Fusion-specific siblings we ignore).
//!
//! SmartOS is **not** cloud-init NoCloud — it provisions guests via
//! the SmartOS metadata service (mdata-get / mdata-put). Including
//! it here is "ouroboros mode": the same machinery that fetches
//! Linux/BSD nocloud images can also turn the upstream SmartOS VM
//! image into a Triton-importable manifest. The `os` field reports
//! `illumos` (matching OmniOS) so consumers don't mistake this for
//! a Triton-native zone image.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256Pinned;

pub struct Smartos;

#[async_trait]
impl VendorProfile for Smartos {
    fn name(&self) -> &str {
        "smartos"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("smartos image url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::VmdkInTarGz,
            os: "illumos".to_string(),
            // Rolling release with no codename or major; flat
            // `smartos` series + the timestamp as version.
            series: "smartos".to_string(),
            version: resolved.release.clone(),
            description: format!(
                "SmartOS {} VM image (does NOT support cloud-init NoCloud; \
                 SmartOS uses mdata-get for guest metadata). Built to run \
                 on bhyve virtual machines.",
                resolved.release
            ),
            homepage: Url::parse("https://smartos.org/").context("smartos homepage url")?,
            ssh_key: false,
            verifier: Box::new(Sha256Pinned(resolved.sha256.clone())),
            expected_sha256: Some(resolved.sha256),
        })
    }
}
