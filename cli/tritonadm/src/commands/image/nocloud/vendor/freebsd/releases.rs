// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FreeBSD release discovery via the VM-IMAGES directory listing.
//!
//! There is no JSON or RFC822 metadata file for FreeBSD releases.
//! The canonical listing is the auto-generated HTML directory index
//! at `https://download.freebsd.org/releases/VM-IMAGES/`. We grep
//! for `<major>.<minor>-RELEASE/` entries and pick the numerically
//! highest one. This is the same algorithm `releng/check-old.sh`
//! and several other FreeBSD release tools use.

use anyhow::{Context, Result};
use regex::Regex;

use crate::commands::image::nocloud::vendor::Release;

const VM_IMAGES_URL: &str = "https://download.freebsd.org/releases/VM-IMAGES/";

pub async fn find_latest(http: &reqwest::Client) -> Result<String> {
    let versions = fetch_versions(http).await?;
    versions
        .first()
        .map(|(maj, min)| format!("{maj}.{min}"))
        .ok_or_else(|| anyhow::anyhow!("no `X.Y-RELEASE/` entries found in {VM_IMAGES_URL}"))
}

/// Enumerate every `<X>.<Y>-RELEASE/` entry in the VM-IMAGES index,
/// newest-first. No EOL signal in the listing.
pub async fn list(http: &reqwest::Client) -> Result<Vec<Release>> {
    Ok(fetch_versions(http)
        .await?
        .into_iter()
        .map(|(maj, min)| Release {
            name: format!("{maj}.{min}"),
            label: Some(format!("FreeBSD {maj}.{min}-RELEASE")),
            note: None,
        })
        .collect())
}

/// Fetch the VM-IMAGES directory listing and return every distinct
/// `(major, minor)` pair newest-first.
async fn fetch_versions(http: &reqwest::Client) -> Result<Vec<(u32, u32)>> {
    eprintln!("Fetching FreeBSD VM-IMAGES directory listing ...");
    let body = http
        .get(VM_IMAGES_URL)
        .send()
        .await
        .with_context(|| format!("GET {VM_IMAGES_URL}"))?
        .error_for_status()
        .with_context(|| format!("status from {VM_IMAGES_URL}"))?
        .text()
        .await
        .with_context(|| format!("read body of {VM_IMAGES_URL}"))?;
    Ok(parse_versions_from_html(&body))
}

fn parse_versions_from_html(body: &str) -> Vec<(u32, u32)> {
    let Ok(re) = Regex::new(r"\b(\d+)\.(\d+)-RELEASE/") else {
        return Vec::new();
    };
    let mut versions: Vec<(u32, u32)> = re
        .captures_iter(body)
        .filter_map(|cap| {
            let major: u32 = cap.get(1)?.as_str().parse().ok()?;
            let minor: u32 = cap.get(2)?.as_str().parse().ok()?;
            Some((major, minor))
        })
        .collect();
    versions.sort();
    versions.dedup();
    versions.reverse();
    versions
}

/// Validate and normalize a user-supplied release token. Accepts
/// `X.Y` and `X.Y-RELEASE` and returns just `X.Y`. Other forms
/// (codenames, "current", patch suffixes like `-p1`) are rejected;
/// FreeBSD doesn't publish VM images for those at the same URL.
pub fn parse_version(input: &str) -> Result<String> {
    let s = input.trim();
    let stripped = s.strip_suffix("-RELEASE").unwrap_or(s);
    let parts: Vec<&str> = stripped.split('.').collect();
    if parts.len() != 2 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
        anyhow::bail!("freebsd: expected version like '15.0' or '15.0-RELEASE', got {input:?}");
    }
    Ok(stripped.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_versions_picks_highest_first() {
        let body = r#"
            <a href="13.5-RELEASE/">13.5-RELEASE/</a>
            <a href="14.3-RELEASE/">14.3-RELEASE/</a>
            <a href="14.4-RELEASE/">14.4-RELEASE/</a>
            <a href="15.0-RELEASE/">15.0-RELEASE/</a>
        "#;
        assert_eq!(parse_versions_from_html(body).first().unwrap(), &(15, 0));
    }

    #[test]
    fn parse_versions_handles_double_digits() {
        let body = "9.3-RELEASE/ 10.4-RELEASE/ 11.4-RELEASE/";
        assert_eq!(parse_versions_from_html(body).first().unwrap(), &(11, 4));
    }

    #[test]
    fn parse_versions_returns_empty_when_no_matches() {
        assert!(parse_versions_from_html("").is_empty());
        assert!(parse_versions_from_html("nothing here").is_empty());
    }

    #[test]
    fn parse_version_accepts_short_form() {
        assert_eq!(parse_version("15.0").unwrap(), "15.0");
    }

    #[test]
    fn parse_version_strips_suffix() {
        assert_eq!(parse_version("15.0-RELEASE").unwrap(), "15.0");
    }

    #[test]
    fn parse_version_rejects_invalid() {
        assert!(parse_version("15").is_err());
        assert!(parse_version("CURRENT").is_err());
        assert!(parse_version("15.0-p1").is_err());
        assert!(parse_version("15.0.1").is_err());
    }

    #[test]
    fn parse_versions_html_sorts_newest_first_and_dedupes() {
        let body = r#"
            <a href="13.5-RELEASE/">13.5-RELEASE/</a>
            <a href="13.5-RELEASE/">13.5-RELEASE/</a>
            <a href="14.4-RELEASE/">14.4-RELEASE/</a>
            <a href="15.0-RELEASE/">15.0-RELEASE/</a>
        "#;
        let rows = parse_versions_from_html(body);
        assert_eq!(rows, vec![(15, 0), (14, 4), (13, 5)]);
    }
}
