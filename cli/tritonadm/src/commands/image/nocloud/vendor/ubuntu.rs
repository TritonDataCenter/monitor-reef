// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Ubuntu cloud-image vendor profile.
//!
//! Resolves a release token (`latest`, `noble`, `24.04`, ...) to a
//! concrete cloud image. The primary path consults Canonical's Simple
//! Streams metadata feed, which gives us the canonical upstream build
//! serial, the exact item URL, and the sha256 in one TLS roundtrip.
//! On streams failure (network, schema change), we fall back to a
//! small hardcoded series table that points at the
//! `<series>/current/` alias and verifies via `SHA256SUMS`.

mod streams;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::{Sha256Pinned, Sha256SumsTls};

pub struct Ubuntu;

/// Fallback series table. Used only if the Simple Streams metadata is
/// unreachable. The third tuple element marks LTS-ness; `latest`
/// resolves to [`LATEST_LTS`] (which must appear in this list).
const SERIES: &[(&str, &str, bool)] = &[
    ("noble", "24.04 LTS Noble Numbat", true),
    ("jammy", "22.04 LTS Jammy Jellyfish", true),
    ("focal", "20.04 LTS Focal Fossa", true),
    ("oracular", "24.10 Oracular Oriole", false),
];

/// Series that `--release latest` resolves to in fallback mode. Bump
/// on each new LTS. The streams path is preferred and self-updating;
/// this is the air-gapped escape hatch.
const LATEST_LTS: &str = "noble";

fn resolve_to_series(release: &str) -> Result<&'static str> {
    let r = release.trim();
    if r == "latest" {
        return Ok(LATEST_LTS);
    }
    if let Some((s, _, _)) = SERIES.iter().find(|(s, _, _)| *s == r) {
        return Ok(*s);
    }
    if let Some((s, _, _)) = SERIES.iter().find(|(_, descr, _)| descr.starts_with(r)) {
        return Ok(*s);
    }
    let known: Vec<&str> = SERIES.iter().map(|(s, _, _)| *s).collect();
    anyhow::bail!(
        "ubuntu: unknown release {r:?}; try one of: {} or 'latest'",
        known.join(", ")
    );
}

#[async_trait]
impl VendorProfile for Ubuntu {
    fn name(&self) -> &str {
        "ubuntu"
    }

    async fn resolve(
        &self,
        release: &str,
        http: &reqwest::Client,
    ) -> Result<ResolvedImage> {
        match resolve_via_streams(release, http).await {
            Ok(r) => Ok(r),
            Err(e) => {
                eprintln!(
                    "warning: Ubuntu Simple Streams resolution failed ({e}); \
                     falling back to hardcoded series table"
                );
                resolve_via_table(release).await
            }
        }
    }
}

async fn resolve_via_streams(
    release: &str,
    http: &reqwest::Client,
) -> Result<ResolvedImage> {
    let index = streams::fetch(http).await?;
    let img = streams::resolve(&index, release)?;

    let description = format!(
        "Ubuntu {} ({}) CloudInit NoCloud compatible image. \
         Built to run on bhyve virtual machines.",
        img.release_title, img.codename
    );

    Ok(ResolvedImage {
        url: img.url,
        format: SourceFormat::Qcow2,
        os: "linux".to_string(),
        series: img.codename,
        // Use the upstream build serial as the manifest version. Two
        // runs against the same upstream produce the same manifest
        // version, which matches what IMGAPI consumers expect.
        version: img.serial,
        description,
        homepage: Url::parse("https://ubuntu.com/").context("ubuntu homepage url")?,
        ssh_key: true,
        verifier: Box::new(Sha256Pinned(img.sha256)),
    })
}

async fn resolve_via_table(release: &str) -> Result<ResolvedImage> {
    let series = resolve_to_series(release)?.to_string();
    let filename = format!("{series}-server-cloudimg-amd64.img");
    let url: Url = format!("https://cloud-images.ubuntu.com/{series}/current/{filename}")
        .parse()
        .context("ubuntu image url")?;
    let sums_url: Url =
        format!("https://cloud-images.ubuntu.com/{series}/current/SHA256SUMS")
            .parse()
            .context("ubuntu SHA256SUMS url")?;

    // Without streams we don't know the upstream serial, so fall back
    // to today's date. Mirrors `target/triton-nocloud-images/build.sh`.
    let version = chrono::Utc::now().format("%Y%m%d").to_string();

    Ok(ResolvedImage {
        url,
        format: SourceFormat::Qcow2,
        os: "linux".to_string(),
        series: series.clone(),
        version,
        description: format!(
            "Ubuntu {series} CloudInit NoCloud compatible image. \
             Built to run on bhyve virtual machines."
        ),
        homepage: Url::parse("https://ubuntu.com/").context("ubuntu homepage url")?,
        ssh_key: true,
        verifier: Box::new(Sha256SumsTls::new(sums_url, filename)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn latest_lts_is_in_series() {
        assert!(SERIES.iter().any(|(s, _, is_lts)| *s == LATEST_LTS && *is_lts));
    }

    #[test]
    fn resolve_latest_to_lts() {
        assert_eq!(resolve_to_series("latest").unwrap(), LATEST_LTS);
    }

    #[test]
    fn resolve_known_series() {
        assert_eq!(resolve_to_series("noble").unwrap(), "noble");
        assert_eq!(resolve_to_series("jammy").unwrap(), "jammy");
    }

    #[test]
    fn resolve_by_version_prefix() {
        assert_eq!(resolve_to_series("24.04").unwrap(), "noble");
        assert_eq!(resolve_to_series("22.04").unwrap(), "jammy");
    }

    #[test]
    fn resolve_unknown_errors() {
        assert!(resolve_to_series("warty").is_err());
    }
}
