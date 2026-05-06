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
//! (`SHA256 (filename) = hex`) — same shape as FreeBSD's
//! CHECKSUM.SHA256, so we reuse `Sha256BsdSumsTls`. We pick the
//! highest-versioned dated `-Base-` build by parsing the directory
//! listing, ignoring the rolling-pointer aliases and the LVM flavor.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256BsdSumsTls;

pub struct Rocky;

#[async_trait]
impl VendorProfile for Rocky {
    fn name(&self) -> &str {
        "rocky"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("rocky image url")?;
        let sidecar_url: Url = resolved
            .sidecar_url
            .parse()
            .context("rocky CHECKSUM sidecar url")?;

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
            verifier: Box::new(Sha256BsdSumsTls::new(sidecar_url, resolved.filename)),
            // CHECKSUM sidecar is fetched at verify time.
            expected_sha256: None,
        })
    }
}
