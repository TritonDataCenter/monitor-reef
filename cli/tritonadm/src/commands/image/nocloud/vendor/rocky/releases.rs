// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Rocky Linux release discovery via the per-major image directory
//! at `https://download.rockylinux.org/pub/rocky/<major>/images/x86_64/`.
//!
//! Rocky publishes both rolling-pointer and dated builds in the same
//! directory:
//!
//! - `Rocky-<major>-GenericCloud-Base.latest.x86_64.qcow2` — pointer
//! - `Rocky-<major>-GenericCloud-Base-<ver>.x86_64.qcow2` — dated
//! - `Rocky-<major>-GenericCloud-LVM-...` — alternate LVM flavor
//!
//! We pick the highest-versioned dated `-Base-` build (lexicographic
//! sort works since we're filtering to a single major). Each dated
//! build has its own `<filename>.CHECKSUM` sidecar in BSD-traditional
//! `SHA256 (filename) = hex` form, which the existing
//! `Sha256BsdSumsTls` verifier handles.

use anyhow::{Context, Result};
use regex::Regex;

use crate::commands::image::nocloud::verify::parse_bsd_sums_file;

const REPO_BASE: &str = "https://download.rockylinux.org/pub/rocky/";

#[derive(Debug)]
pub struct Resolved {
    pub major: String,
    /// Filename minus the `Rocky-<major>-GenericCloud-Base-` prefix
    /// and `.x86_64.qcow2` suffix (e.g. `9.7-20251123.2`). Used as
    /// the manifest version.
    pub build: String,
    pub url: String,
    /// Upstream sha256 from the per-file `.CHECKSUM` sidecar,
    /// fetched at resolve time so `--dry-run` can show the
    /// derived manifest UUID without downloading the qcow2.
    pub sha256: String,
}

pub async fn resolve(http: &reqwest::Client, release: &str) -> Result<Resolved> {
    let token = release.trim();
    let major = if token.eq_ignore_ascii_case("latest") {
        let majors = fetch_majors(http).await?;
        majors
            .iter()
            .max()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no Rocky majors found at {REPO_BASE}"))?
            .to_string()
    } else {
        parse_major(token)?
    };

    let images_base = format!("https://download.rockylinux.org/pub/rocky/{major}/images/x86_64/");
    eprintln!("Fetching Rocky {major} image listing ...");
    let body = http
        .get(&images_base)
        .send()
        .await
        .with_context(|| format!("GET {images_base}"))?
        .error_for_status()
        .with_context(|| format!("status from {images_base}"))?
        .text()
        .await
        .with_context(|| format!("read body of {images_base}"))?;

    let filename = find_latest_dated(&body, &major).ok_or_else(|| {
        anyhow::anyhow!("no dated `Rocky-{major}-GenericCloud-Base-…` qcow2 in {images_base}")
    })?;
    let build = strip_filename_chrome(&filename, &major).unwrap_or_else(|| filename.clone());

    let url = format!("{images_base}{filename}");
    let sidecar_url = format!("{url}.CHECKSUM");
    let sha256 = fetch_sidecar_hash(http, &sidecar_url, &filename).await?;

    Ok(Resolved {
        major,
        build,
        url,
        sha256,
    })
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
    parse_bsd_sums_file(&body, filename)
        .ok_or_else(|| anyhow::anyhow!("CHECKSUM at {sidecar_url} has no entry for {filename}"))
}

async fn fetch_majors(http: &reqwest::Client) -> Result<Vec<u32>> {
    eprintln!("Fetching Rocky directory listing ...");
    let body = http
        .get(REPO_BASE)
        .send()
        .await
        .with_context(|| format!("GET {REPO_BASE}"))?
        .error_for_status()
        .with_context(|| format!("status from {REPO_BASE}"))?
        .text()
        .await
        .with_context(|| format!("read body of {REPO_BASE}"))?;
    Ok(parse_majors_from_html(&body))
}

fn parse_majors_from_html(body: &str) -> Vec<u32> {
    let re = match Regex::new(r#"href="(\d+)/""#) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut majors: Vec<u32> = re
        .captures_iter(body)
        .filter_map(|c| c.get(1)?.as_str().parse().ok())
        .collect();
    majors.sort();
    majors.dedup();
    majors
}

fn parse_major(input: &str) -> Result<String> {
    let s = input.trim();
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("rocky: expected a major version like '9' or 'latest', got {input:?}");
    }
    Ok(s.to_string())
}

/// Find the highest-versioned dated `-Base-` build in an image
/// directory listing. Lex sort works because all candidates share
/// the same major and the build suffix is `<minor>.<minor>-<date>.<n>`.
fn find_latest_dated(body: &str, major: &str) -> Option<String> {
    let prefix = format!("Rocky-{major}-GenericCloud-Base-");
    let suffix = ".x86_64.qcow2";
    let mut candidates: Vec<&str> = body
        .split(['"', '<', '>', ' ', '\n'])
        .filter(|name| {
            name.starts_with(&prefix) && name.ends_with(suffix) && !name.ends_with(".CHECKSUM")
        })
        .filter(|name| {
            // Skip the rolling pointer that lives in the same prefix
            // namespace ("Rocky-N-GenericCloud-Base.latest.x86_64.qcow2"
            // — no leading dash after Base, but a `.` instead).
            let middle = &name[prefix.len()..name.len() - suffix.len()];
            !middle.contains(".latest")
        })
        .collect();
    candidates.sort();
    candidates.dedup();
    candidates.last().map(|s| s.to_string())
}

fn strip_filename_chrome(filename: &str, major: &str) -> Option<String> {
    let prefix = format!("Rocky-{major}-GenericCloud-Base-");
    let stripped = filename.strip_prefix(&prefix)?;
    let stripped = stripped.strip_suffix(".x86_64.qcow2")?;
    Some(stripped.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_majors_extracts_numeric_dirs() {
        let body = r#"href="8/" href="9/" href="10/" href="vault/""#;
        assert_eq!(parse_majors_from_html(body), vec![8, 9, 10]);
    }

    #[test]
    fn parse_major_accepts_integer_only() {
        assert_eq!(parse_major("9").unwrap(), "9");
        assert!(parse_major("9.7").is_err());
        assert!(parse_major("nine").is_err());
        assert!(parse_major("").is_err());
    }

    #[test]
    fn find_latest_dated_picks_highest_base_build() {
        let body = r#"
            <a href="Rocky-9-GenericCloud-Base-9.6-20250410.0.x86_64.qcow2">…</a>
            <a href="Rocky-9-GenericCloud-Base-9.7-20251123.2.x86_64.qcow2">…</a>
            <a href="Rocky-9-GenericCloud-Base-9.7-20250901.0.x86_64.qcow2">…</a>
            <a href="Rocky-9-GenericCloud-Base.latest.x86_64.qcow2">…</a>
            <a href="Rocky-9-GenericCloud-LVM-9.7-20251123.2.x86_64.qcow2">…</a>
            <a href="Rocky-9-GenericCloud.latest.x86_64.qcow2">…</a>
            <a href="Rocky-9-GenericCloud-Base-9.7-20251123.2.x86_64.qcow2.CHECKSUM">…</a>
        "#;
        assert_eq!(
            find_latest_dated(body, "9").unwrap(),
            "Rocky-9-GenericCloud-Base-9.7-20251123.2.x86_64.qcow2"
        );
    }

    #[test]
    fn find_latest_dated_returns_none_when_only_pointers_present() {
        let body = "Rocky-9-GenericCloud-Base.latest.x86_64.qcow2";
        assert!(find_latest_dated(body, "9").is_none());
    }

    #[test]
    fn strip_chrome_extracts_build() {
        assert_eq!(
            strip_filename_chrome("Rocky-9-GenericCloud-Base-9.7-20251123.2.x86_64.qcow2", "9"),
            Some("9.7-20251123.2".to_string())
        );
        assert_eq!(
            strip_filename_chrome("Rocky-9-GenericCloud-LVM-9.7-x.qcow2", "9"),
            None
        );
    }
}
