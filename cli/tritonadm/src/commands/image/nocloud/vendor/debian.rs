// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Debian cloud-image vendor profile.
//!
//! Debian publishes generic cloud images at
//! `https://cloud.debian.org/images/cloud/<codename>/latest/`. We pick
//! the `genericcloud` qcow2 — its cloud-init auto-detects the NoCloud
//! datasource SmartOS provides on bhyve. The sibling `SHA512SUMS`
//! file is published in SHA-512 (not SHA-256), so we use the
//! `Sha512SumsTls` verifier rather than the SHA-256 path used by
//! Ubuntu's fallback.
//!
//! Release resolution consults Debian's apt `Release` file at
//! `https://deb.debian.org/debian/dists/<suite>/Release` — the same
//! file apt itself uses to know what `stable` means today. This lets
//! the user pass any of:
//!
//! - `latest` — alias for `stable`
//! - symbolic suite names — `stable`, `oldstable`, `oldoldstable`,
//!   `testing`, `unstable`
//! - codenames — `trixie`, `bookworm`, `bullseye`, `forky`, `sid`, ...
//!
//! Since the build downloads a multi-hundred-megabyte image over the
//! same network, requiring an additional small Release-file fetch
//! adds no real fragility, and we use its `Codename` and `Version`
//! fields directly. No hardcoded codename table.

mod release_file;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha512SumsTls;

pub struct Debian;

/// Translate the user-facing release token to the suite path component
/// used in the apt Release URL `dists/<suite>/Release`. `latest` is
/// the only alias we own; everything else passes through unchanged
/// and either resolves at upstream or 404s with a clear error.
fn token_to_suite(release: &str) -> &str {
    match release.trim() {
        "latest" => "stable",
        other => other,
    }
}

#[async_trait]
impl VendorProfile for Debian {
    fn name(&self) -> &str {
        "debian"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let suite = token_to_suite(release);
        let info = release_file::fetch(http, suite)
            .await
            .with_context(|| format!("resolve debian {release:?}"))?;

        let codename = info.codename;
        let version = info.version;
        let major = release_file::major_of(&version).ok_or_else(|| {
            anyhow::anyhow!(
                "could not parse major version from upstream {version:?} for {codename}"
            )
        })?;

        let filename = format!("debian-{major}-genericcloud-amd64.qcow2");
        let url: Url =
            format!("https://cloud.debian.org/images/cloud/{codename}/latest/{filename}")
                .parse()
                .context("debian image url")?;
        let sums_url: Url =
            format!("https://cloud.debian.org/images/cloud/{codename}/latest/SHA512SUMS")
                .parse()
                .context("debian SHA512SUMS url")?;

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            series: codename.clone(),
            // Use Debian's point-release version (e.g. "13.4") as the
            // manifest version. Two builds against the same point
            // release produce the same manifest version, while a
            // point-release upgrade produces a new one.
            version: version.clone(),
            description: format!(
                "Debian {version} ({codename}) CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines."
            ),
            homepage: Url::parse("https://www.debian.org/").context("debian homepage url")?,
            ssh_key: true,
            verifier: Box::new(Sha512SumsTls::new(sums_url, filename)),
            // Debian's hash channel is SHA-512, not SHA-256, so the
            // pre-download UUID derivation isn't possible. The dry-run
            // plan will show "(derived after download)".
            expected_sha256: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_aliases_to_stable() {
        assert_eq!(token_to_suite("latest"), "stable");
    }

    #[test]
    fn codename_passes_through() {
        assert_eq!(token_to_suite("trixie"), "trixie");
        assert_eq!(token_to_suite("bookworm"), "bookworm");
    }

    #[test]
    fn symbolic_suites_pass_through() {
        assert_eq!(token_to_suite("stable"), "stable");
        assert_eq!(token_to_suite("oldstable"), "oldstable");
        assert_eq!(token_to_suite("oldoldstable"), "oldoldstable");
        assert_eq!(token_to_suite("testing"), "testing");
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(token_to_suite("  trixie  "), "trixie");
    }
}
