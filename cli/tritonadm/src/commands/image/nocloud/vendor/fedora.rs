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

use super::{PinnedQcow2, ResolvedImage, VendorProfile};

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
        PinnedQcow2 {
            url,
            // Fedora has no codenames after F20 — major is the
            // canonical short name used everywhere in the ecosystem.
            series: format!("f{}", resolved.major),
            // Build serial (e.g. `44-1.7`) so distinct rebuilds of
            // the same major don't collide in the output filenames.
            version: resolved.build,
            description: format!(
                "Fedora {} Cloud Base CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.major
            ),
            homepage: "https://fedoraproject.org/",
            sha256: resolved.sha256,
        }
        .into_resolved("fedora")
    }
}
