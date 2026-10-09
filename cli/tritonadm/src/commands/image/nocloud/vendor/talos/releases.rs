// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Talos release discovery via the GitHub releases API.
//!
//! Talos doesn't publish a Simple-Streams-style metadata feed; the
//! canonical "what is the current stable version" signal is the
//! upstream GitHub release at
//! `https://api.github.com/repos/siderolabs/talos/releases/latest`.
//! We strip the `v` prefix from the `tag_name` field for use in the
//! factory URL.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::commands::image::nocloud::vendor::Release;

const LATEST_URL: &str = "https://api.github.com/repos/siderolabs/talos/releases/latest";
const RELEASES_URL: &str = "https://api.github.com/repos/siderolabs/talos/releases?per_page=30";

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

/// Talos publishes ~one minor every couple of months and patches them
/// for a year, so the most recent 30 releases comfortably cover every
/// minor we'd want to pin. We filter out drafts and prereleases —
/// those don't have factory builds.
pub async fn list(http: &reqwest::Client) -> Result<Vec<Release>> {
    eprintln!("Fetching Talos releases from GitHub ...");
    let payload: Vec<GitHubRelease> = http
        .get(RELEASES_URL)
        .header("User-Agent", "tritonadm-fetch-nocloud")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("GET {RELEASES_URL}"))?
        .error_for_status()
        .with_context(|| format!("status from {RELEASES_URL}"))?
        .json()
        .await
        .with_context(|| format!("parse {RELEASES_URL}"))?;
    Ok(payload
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let stripped = r.tag_name.strip_prefix('v').unwrap_or(&r.tag_name);
            // Skip tags that aren't semver `X.Y.Z`.
            if parse_version(stripped).is_err() {
                return None;
            }
            Some(Release {
                name: stripped.to_string(),
                label: Some(format!("Talos v{stripped}")),
                note: None,
            })
        })
        .collect())
}

pub async fn find_latest(http: &reqwest::Client) -> Result<String> {
    eprintln!("Fetching Talos latest release from GitHub ...");
    // GitHub returns 403 to requests without User-Agent.
    let resp = http
        .get(LATEST_URL)
        .header("User-Agent", "tritonadm-fetch-nocloud")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("GET {LATEST_URL}"))?
        .error_for_status()
        .with_context(|| format!("status from {LATEST_URL}"))?
        .json::<GitHubRelease>()
        .await
        .with_context(|| format!("parse {LATEST_URL}"))?;
    Ok(resp
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&resp.tag_name)
        .to_string())
}

/// Validate and normalize a user-supplied version. Accepts `X.Y.Z`
/// and `vX.Y.Z`, returns `X.Y.Z`. Talos uses semver releases.
pub fn parse_version(input: &str) -> Result<String> {
    let s = input.trim();
    let stripped = s.strip_prefix('v').unwrap_or(s);
    let parts: Vec<&str> = stripped.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
        anyhow::bail!("talos: expected version like '1.12.7' or 'v1.12.7', got {input:?}");
    }
    Ok(stripped.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_accepts_short_and_v_form() {
        assert_eq!(parse_version("1.12.7").unwrap(), "1.12.7");
        assert_eq!(parse_version("v1.12.7").unwrap(), "1.12.7");
    }

    #[test]
    fn parse_version_rejects_invalid() {
        assert!(parse_version("1.12").is_err());
        assert!(parse_version("1.12.7-rc1").is_err());
        assert!(parse_version("latest").is_err());
        assert!(parse_version("garbage").is_err());
    }
}
