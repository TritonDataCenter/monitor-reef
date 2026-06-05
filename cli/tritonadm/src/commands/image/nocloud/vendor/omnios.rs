// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! OmniOS cloud-image vendor profile.
//!
//! OmniOS publishes a single `omnios-<id>.cloud.vmdk` per release
//! channel at `https://downloads.omnios.org/media/<channel>/`,
//! with channels `stable`, `lts`, and `bloody`. Each ships a
//! sibling bare-hash `<file>.sha256` sidecar.
//!
//! The release-resolution path here is fully wired (pre-fetches
//! the sha256 so `--dry-run` shows the manifest UUID); the actual
//! VMDK-to-zvol conversion is deferred. The `SourceFormat::Vmdk`
//! arm in the pipeline bails clearly until a vendored vmdk reader
//! is in place.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256Pinned;

pub struct Omnios;

#[async_trait]
impl VendorProfile for Omnios {
    fn name(&self) -> &str {
        "omnios"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("omnios image url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Vmdk,
            os: "illumos".to_string(),
            series: resolved.channel.clone(),
            version: resolved.build.clone(),
            description: format!(
                "OmniOS {} {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.channel, resolved.build
            ),
            homepage: Url::parse("https://omnios.org/").context("omnios homepage url")?,
            ssh_key: true,
            verifier: Box::new(Sha256Pinned(resolved.sha256.clone())),
            expected_sha256: Some(resolved.sha256),
        })
    }
}
