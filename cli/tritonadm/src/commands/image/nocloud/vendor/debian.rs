// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Debian cloud-image vendor profile.
//!
//! Debian publishes generic cloud images under
//! `https://cloud.debian.org/images/cloud/<codename>/latest/`. The
//! `latest/` directory is a stable alias for the most recent build of
//! that codename. We pick the `genericcloud` qcow2 — its cloud-init
//! is configured to auto-detect the NoCloud datasource SmartOS
//! provides on bhyve. The sibling `SHA512SUMS` file is published in
//! SHA-512 (not SHA-256), so we use the `Sha512SumsTls` verifier
//! rather than the SHA-256 path used by Ubuntu's fallback.
//!
//! Debian doesn't publish a Simple Streams metadata feed, so release
//! resolution is table-driven. The version field of the manifest is
//! the build date because the `latest/` URL doesn't expose the
//! upstream serial — improving this is a follow-up.

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{ResolvedImage, SourceFormat, VendorProfile};
use crate::commands::image::nocloud::verify::Sha512SumsTls;

pub struct Debian;

/// Debian releases. Order matters only insofar as `latest` resolves
/// to [`LATEST_STABLE`]. The major number is the integer used in the
/// upstream filename pattern (`debian-<major>-genericcloud-amd64.qcow2`).
const RELEASES: &[(&str, u32, &str)] = &[
    ("trixie", 13, "13 Trixie (current stable)"),
    ("bookworm", 12, "12 Bookworm (oldstable)"),
    ("bullseye", 11, "11 Bullseye (older oldstable, LTS)"),
];

/// Codename that `--release latest` resolves to. Bump when Debian
/// promotes a new release to stable.
const LATEST_STABLE: &str = "trixie";

fn resolve_to_release(release: &str) -> Result<(&'static str, u32, &'static str)> {
    let r = release.trim();
    if r == "latest" {
        return RELEASES
            .iter()
            .find(|(c, _, _)| *c == LATEST_STABLE)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("LATEST_STABLE {LATEST_STABLE} not in RELEASES"));
    }
    if let Some(entry) = RELEASES.iter().find(|(c, _, _)| *c == r) {
        return Ok(*entry);
    }
    if let Ok(major) = r.parse::<u32>()
        && let Some(entry) = RELEASES.iter().find(|(_, m, _)| *m == major)
    {
        return Ok(*entry);
    }
    let known: Vec<&str> = RELEASES.iter().map(|(c, _, _)| *c).collect();
    anyhow::bail!(
        "debian: unknown release {r:?}; try one of: {} or 'latest'",
        known.join(", ")
    );
}

#[async_trait]
impl VendorProfile for Debian {
    fn name(&self) -> &str {
        "debian"
    }

    async fn resolve(
        &self,
        release: &str,
        _http: &reqwest::Client,
    ) -> Result<ResolvedImage> {
        let (codename, major, descr) = resolve_to_release(release)?;
        let filename = format!("debian-{major}-genericcloud-amd64.qcow2");
        let url: Url =
            format!("https://cloud.debian.org/images/cloud/{codename}/latest/{filename}")
                .parse()
                .context("debian image url")?;
        let sums_url: Url =
            format!("https://cloud.debian.org/images/cloud/{codename}/latest/SHA512SUMS")
                .parse()
                .context("debian SHA512SUMS url")?;

        // Build serial isn't trivially derivable from the `latest/` URL
        // (the directory listing has it, but parsing HTML is fragile).
        // Mirror our Ubuntu-fallback behavior: today's date.
        let version = chrono::Utc::now().format("%Y%m%d").to_string();

        Ok(ResolvedImage {
            url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            series: codename.to_string(),
            version,
            description: format!(
                "Debian {descr} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines."
            ),
            homepage: Url::parse("https://www.debian.org/")
                .context("debian homepage url")?,
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn latest_stable_is_in_releases() {
        assert!(RELEASES.iter().any(|(c, _, _)| *c == LATEST_STABLE));
    }

    #[test]
    fn resolve_latest() {
        let (codename, _, _) = resolve_to_release("latest").unwrap();
        assert_eq!(codename, LATEST_STABLE);
    }

    #[test]
    fn resolve_codename() {
        let (codename, major, _) = resolve_to_release("trixie").unwrap();
        assert_eq!(codename, "trixie");
        assert_eq!(major, 13);
    }

    #[test]
    fn resolve_by_major() {
        let (codename, major, _) = resolve_to_release("12").unwrap();
        assert_eq!(codename, "bookworm");
        assert_eq!(major, 12);
    }

    #[test]
    fn resolve_unknown_errors() {
        assert!(resolve_to_release("warty").is_err());
        assert!(resolve_to_release("99").is_err());
    }
}
