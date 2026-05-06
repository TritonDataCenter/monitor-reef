// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Fedora cloud-image vendor profile.
//!
//! Fedora publishes Cloud_Base qcow2 images at
//! `https://download.fedoraproject.org/pub/fedora/linux/releases/<n>/Cloud/x86_64/images/`.
//! Release discovery uses `https://fedoraproject.org/releases.json`,
//! which lists every shipping artifact (variant × subvariant × arch ×
//! format) with the upstream sha256 inline — same shape as Ubuntu
//! Simple Streams, so we use a plain `Sha256Pinned` verifier.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256Pinned;

pub struct Fedora;

#[async_trait]
impl VendorProfile for Fedora {
    fn name(&self) -> &str {
        "fedora"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let entries = releases::fetch(http).await?;
        let resolved = releases::resolve(&entries, release)?;

        let url: Url = resolved.url.parse().context("fedora image url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            // Fedora has no codenames after F20 — major is the
            // canonical short name used everywhere in the ecosystem.
            series: format!("f{}", resolved.major),
            // Build serial (e.g. `44-1.7`) so distinct rebuilds of
            // the same major don't collide in the output filenames.
            version: resolved.build.clone(),
            description: format!(
                "Fedora {} Cloud Base CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.major
            ),
            homepage: Url::parse("https://fedoraproject.org/").context("fedora homepage url")?,
            ssh_key: true,
            verifier: Box::new(Sha256Pinned(resolved.sha256.clone())),
            expected_sha256: Some(resolved.sha256),
        })
    }
}
