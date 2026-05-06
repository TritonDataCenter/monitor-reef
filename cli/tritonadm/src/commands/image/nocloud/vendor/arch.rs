// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Arch Linux cloud-image vendor profile.
//!
//! Arch publishes per-build cloudimg directories at
//! `https://geo.mirror.pkgbuild.com/images/`. Each `v<date>.<build>/`
//! has a dated `Arch-Linux-x86_64-cloudimg-<date>.<build>.qcow2`
//! and a Linux-style `<file>.SHA256` sidecar; we fetch the sidecar
//! at resolve time and pin the hash. Detached GPG signatures live
//! alongside (`<file>.sig`, `<file>.SHA256.sig`) but the POC trusts
//! TLS only — same floor as the other Sums-based vendors.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha256Pinned;

pub struct Arch;

#[async_trait]
impl VendorProfile for Arch {
    fn name(&self) -> &str {
        "arch"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("arch image url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            // Arch is a rolling release — no codenames or majors.
            // Series is the literal `rolling` so manifest names read
            // `arch-rolling-nocloud` rather than `arch-arch-nocloud`;
            // the build identifier (e.g. `20260501.523211`) carries
            // the per-build distinction in `version`.
            series: "rolling".to_string(),
            version: resolved.build.clone(),
            description: format!(
                "Arch Linux {} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.build
            ),
            homepage: Url::parse("https://archlinux.org/").context("arch homepage url")?,
            ssh_key: true,
            verifier: Box::new(Sha256Pinned(resolved.sha256.clone())),
            expected_sha256: Some(resolved.sha256),
        })
    }
}
