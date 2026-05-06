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

use super::{PinnedQcow2, ResolvedImage, VendorProfile};

pub struct CentosStream;

#[async_trait]
impl VendorProfile for CentosStream {
    fn name(&self) -> &str {
        "centos-stream"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("centos-stream image url")?;
        PinnedQcow2 {
            url,
            series: format!("centos{}", resolved.stream),
            version: resolved.build,
            description: format!(
                "CentOS Stream {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.stream
            ),
            homepage: "https://www.centos.org/",
            sha256: resolved.sha256,
        }
        .into_resolved("centos-stream")
    }
}
