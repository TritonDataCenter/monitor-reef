// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! openSUSE Leap release discovery.
//!
//! `download.opensuse.org` runs MirrorCache, which exposes a JSON
//! directory listing via the `?json=1` query — we use it for both
//! the Leap version index and per-version appliance listings.
//!
//! Filename naming changed between Leap 15.x and 16.x:
//!
//! - 15.x: `openSUSE-Leap-<X.Y>-Minimal-VM.x86_64-<X.Y.Z>-Cloud-Build<n>.<m>.qcow2`
//! - 16.x: `Leap-<X.Y>-Minimal-VM.x86_64-Cloud-Build<n>.<m>.qcow2`
//!
//! Both have a sibling `.sha256` sidecar in Linux-style form
//! (`<hex>  <filename>`) and a `.sha256.asc` detached signature.
//! We pick the highest-versioned `Cloud-Build…` qcow2 (skipping
//! the rolling pointer that has no `Build` tag) and pin the hash
//! from the sidecar.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::commands::image::nocloud::verify::parse_sums_file;

const LEAP_BASE: &str = "https://download.opensuse.org/distribution/leap/";

#[derive(Debug, Deserialize)]
struct DirEntry {
    name: String,
}

#[derive(Debug)]
pub struct Resolved {
    pub leap_version: String,
    /// Build identifier (e.g. `16.0-Build16.2` or `15.6.0-Build19.143`)
    /// — used as the manifest version.
    pub build: String,
    pub url: String,
    pub sha256: String,
}

pub async fn resolve(http: &reqwest::Client, release: &str) -> Result<Resolved> {
    let token = release.trim();
    let candidate_versions: Vec<String> = if token.eq_ignore_ascii_case("latest") {
        let mut versions = fetch_leap_versions(http).await?;
        versions.sort_by(|a, b| compare_version_keys(b, a));
        versions
    } else {
        vec![parse_leap_version(token)?]
    };

    let mut last_err: Option<anyhow::Error> = None;
    for version in &candidate_versions {
        match find_in_version(http, version).await {
            Ok(r) => return Ok(r),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no Leap versions returned by {LEAP_BASE}")))
}

async fn find_in_version(http: &reqwest::Client, version: &str) -> Result<Resolved> {
    let appliances_base = format!("{LEAP_BASE}{version}/appliances/");
    let entries = fetch_dir_json(http, &format!("{appliances_base}?json=1")).await?;
    let filename = pick_cloud_build(&entries, version).ok_or_else(|| {
        anyhow::anyhow!("no `Cloud-Build…x86_64.qcow2` entry under {appliances_base}")
    })?;
    let url = format!("{appliances_base}{filename}");
    let sidecar_url = format!("{url}.sha256");
    let sha256 = fetch_sidecar_hash(http, &sidecar_url, &filename).await?;
    let build = build_id(&filename, version).unwrap_or_else(|| filename.clone());

    Ok(Resolved {
        leap_version: version.to_string(),
        build,
        url,
        sha256,
    })
}

async fn fetch_leap_versions(http: &reqwest::Client) -> Result<Vec<String>> {
    let url = format!("{LEAP_BASE}?json=1");
    eprintln!("Fetching openSUSE Leap version index ...");
    let entries = fetch_dir_json(http, &url).await?;
    let versions: Vec<String> = entries
        .iter()
        .filter_map(|e| e.name.strip_suffix('/'))
        .filter(|name| {
            let parts: Vec<&str> = name.split('.').collect();
            parts.len() == 2 && parts.iter().all(|p| p.parse::<u32>().is_ok())
        })
        .map(String::from)
        .collect();
    if versions.is_empty() {
        anyhow::bail!("no `<X>.<Y>/` Leap directories at {url}");
    }
    Ok(versions)
}

async fn fetch_dir_json(http: &reqwest::Client, url: &str) -> Result<Vec<DirEntry>> {
    let body = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("status from {url}"))?
        .text()
        .await
        .with_context(|| format!("read body of {url}"))?;
    serde_json::from_str(&body).with_context(|| format!("parse {url}"))
}

async fn fetch_sidecar_hash(
    http: &reqwest::Client,
    sidecar_url: &str,
    filename: &str,
) -> Result<String> {
    eprintln!("Fetching {sidecar_url}");
    let body = http
        .get(sidecar_url)
        .send()
        .await
        .with_context(|| format!("GET {sidecar_url}"))?
        .error_for_status()
        .with_context(|| format!("status from {sidecar_url}"))?
        .text()
        .await
        .with_context(|| format!("read body of {sidecar_url}"))?;
    parse_sums_file(&body, filename)
        .ok_or_else(|| anyhow::anyhow!("sha256 at {sidecar_url} has no entry for {filename}"))
}

/// Find a `…Leap-<ver>-Minimal-VM.x86_64-…Cloud-Build<n>.<m>.qcow2`
/// entry, skipping the rolling pointer (no `Build` tag) and any
/// non-Cloud / non-x86_64 flavors.
fn pick_cloud_build(entries: &[DirEntry], leap_version: &str) -> Option<String> {
    let prefix_old = format!("openSUSE-Leap-{leap_version}-Minimal-VM.x86_64-");
    let prefix_new = format!("Leap-{leap_version}-Minimal-VM.x86_64-");
    let mut best: Option<&str> = None;
    for entry in entries {
        let name: &str = entry.name.trim_end_matches('/');
        if !name.ends_with(".qcow2") {
            continue;
        }
        let matches_prefix = name.starts_with(&prefix_old) || name.starts_with(&prefix_new);
        if !matches_prefix {
            continue;
        }
        if !name.contains("-Cloud-Build") {
            continue;
        }
        match best {
            Some(b) if b >= name => {}
            _ => best = Some(name),
        }
    }
    best.map(String::from)
}

/// Extract the build identifier (e.g. `16.0-Build16.2` for Leap 16,
/// `15.6.0-Build19.143` for Leap 15.6). Leap 16 names go straight
/// from the prefix into `Cloud-Build…`, while Leap 15 has an inner
/// version (`15.6.0-Cloud-Build…`); we split on `Cloud-` and stitch
/// whichever side is non-empty back to the leading `<X.Y>`.
fn build_id(filename: &str, leap_version: &str) -> Option<String> {
    let prefix_old = format!("openSUSE-Leap-{leap_version}-Minimal-VM.x86_64-");
    let prefix_new = format!("Leap-{leap_version}-Minimal-VM.x86_64-");
    let inner = filename
        .strip_prefix(&prefix_old)
        .or_else(|| filename.strip_prefix(&prefix_new))?;
    let inner = inner.strip_suffix(".qcow2")?;
    let (head, tail) = inner.split_once("Cloud-")?;
    let head = head.trim_end_matches('-');
    if head.is_empty() {
        Some(format!("{leap_version}-{tail}"))
    } else {
        Some(format!("{head}-{tail}"))
    }
}

fn parse_leap_version(input: &str) -> Result<String> {
    let s = input
        .trim()
        .strip_prefix("Leap-")
        .unwrap_or_else(|| input.trim());
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 2 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
        anyhow::bail!("opensuse: expected a Leap version like '15.6' or '16.0', got {input:?}");
    }
    Ok(s.to_string())
}

fn compare_version_keys(a: &str, b: &str) -> std::cmp::Ordering {
    fn key(v: &str) -> (u32, u32) {
        let mut parts = v.split('.');
        let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        (major, minor)
    }
    key(a).cmp(&key(b))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn entries(names: &[&str]) -> Vec<DirEntry> {
        names
            .iter()
            .map(|n| DirEntry {
                name: (*n).to_string(),
            })
            .collect()
    }

    #[test]
    fn pick_cloud_build_handles_leap_16_naming() {
        let dir = entries(&[
            "Leap-16.0-Minimal-VM.x86_64-Cloud-Build16.2.qcow2",
            "Leap-16.0-Minimal-VM.x86_64-Cloud.qcow2",
            "Leap-16.0-Minimal-VM.x86_64-kvm-and-xen-Build16.2.qcow2",
            "Leap-16.0-Minimal-VM.aarch64-Cloud-Build16.2.qcow2",
        ]);
        assert_eq!(
            pick_cloud_build(&dir, "16.0").unwrap(),
            "Leap-16.0-Minimal-VM.x86_64-Cloud-Build16.2.qcow2"
        );
    }

    #[test]
    fn pick_cloud_build_handles_leap_15_naming() {
        let dir = entries(&[
            "openSUSE-Leap-15.6-Minimal-VM.x86_64-15.6.0-Cloud-Build19.143.qcow2",
            "openSUSE-Leap-15.6-Minimal-VM.x86_64-Cloud.qcow2",
            "openSUSE-Leap-15.6-Minimal-VM.x86_64-kvm-and-xen-Build19.143.qcow2",
        ]);
        assert_eq!(
            pick_cloud_build(&dir, "15.6").unwrap(),
            "openSUSE-Leap-15.6-Minimal-VM.x86_64-15.6.0-Cloud-Build19.143.qcow2"
        );
    }

    #[test]
    fn pick_cloud_build_returns_none_when_only_pointer() {
        let dir = entries(&["Leap-16.0-Minimal-VM.x86_64-Cloud.qcow2"]);
        assert!(pick_cloud_build(&dir, "16.0").is_none());
    }

    #[test]
    fn build_id_strips_leap_16_chrome() {
        assert_eq!(
            build_id("Leap-16.0-Minimal-VM.x86_64-Cloud-Build16.2.qcow2", "16.0").unwrap(),
            "16.0-Build16.2"
        );
    }

    #[test]
    fn build_id_strips_leap_15_chrome() {
        assert_eq!(
            build_id(
                "openSUSE-Leap-15.6-Minimal-VM.x86_64-15.6.0-Cloud-Build19.143.qcow2",
                "15.6"
            )
            .unwrap(),
            "15.6.0-Build19.143"
        );
    }

    #[test]
    fn parse_leap_version_accepts_dotted_form() {
        assert_eq!(parse_leap_version("15.6").unwrap(), "15.6");
        assert_eq!(parse_leap_version("Leap-16.0").unwrap(), "16.0");
        assert!(parse_leap_version("15").is_err());
        assert!(parse_leap_version("nine.six").is_err());
        assert!(parse_leap_version("").is_err());
    }

    #[test]
    fn compare_version_keys_orders_numerically() {
        let mut versions = vec!["15.6".to_string(), "16.0".to_string(), "15.5".to_string()];
        versions.sort_by(|a, b| compare_version_keys(b, a));
        assert_eq!(
            versions,
            vec!["16.0".to_string(), "15.6".to_string(), "15.5".to_string()]
        );
    }
}
