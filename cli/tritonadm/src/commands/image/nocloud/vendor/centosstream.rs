// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! CentOS Stream cloud-image vendor profile.
//!
//! CentOS Stream publishes GenericCloud qcow2 images per stream at
//! `https://cloud.centos.org/centos/<n>-stream/x86_64/images/`,
//! with a per-file BSD-style `<filename>.SHA256SUM` sidecar. We
//! list the directory, pick the highest-versioned dated build,
//! pre-fetch the sidecar at resolve time, and pin the hash so
//! `--dry-run` shows the manifest UUID.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256Pinned;

pub struct CentosStream;

#[async_trait]
impl VendorProfile for CentosStream {
    fn name(&self) -> &str {
        "centos-stream"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("centos-stream image url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            series: format!("centos{}", resolved.stream),
            version: resolved.build.clone(),
            description: format!(
                "CentOS Stream {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.stream
            ),
            homepage: Url::parse("https://www.centos.org/")
                .context("centos-stream homepage url")?,
            ssh_key: true,
            verifier: Box::new(Sha256Pinned(resolved.sha256.clone())),
            expected_sha256: Some(resolved.sha256),
        })
    }
}
