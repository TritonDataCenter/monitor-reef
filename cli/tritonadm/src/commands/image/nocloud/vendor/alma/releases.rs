// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! AlmaLinux release discovery.
//!
//! AlmaLinux publishes one cloud-image directory per major version
//! at `https://repo.almalinux.org/almalinux/<major>/cloud/x86_64/images/`.
//! The auto-generated index page at `/almalinux/` lists the
//! directories — we grep it for `<digit>+/` entries to enumerate
//! supported majors. Inside each images directory the rolling pointer
//! `AlmaLinux-<major>-GenericCloud-latest.x86_64.qcow2` always
//! exists; the sibling `CHECKSUM` file is Linux-style
//! (`<sha256>  <filename>`), so the pinned-hash for the latest
//! pointer can be looked up in the same file as the dated build it
//! aliases.

use anyhow::{Context, Result};
use regex::Regex;

const REPO_BASE: &str = "https://repo.almalinux.org/almalinux/";

#[derive(Debug)]
pub struct Resolved {
    /// Major version (e.g. `9`).
    pub major: String,
    /// Filename minus the leading `AlmaLinux-<major>-GenericCloud-`
    /// and trailing `.x86_64.qcow2` — used as the manifest version
    /// (e.g. `9.7-20260501`).
    pub build: String,
    pub sha256: String,
    pub url: String,
}

/// Resolve a release token. `latest` picks the highest major from
/// the directory listing; an integer (`8`, `9`, `10`) pins to that
/// major. The result always carries the dated form (resolved via the
/// CHECKSUM file) so distinct rebuilds dedupe in the manifest.
pub async fn resolve(http: &reqwest::Client, release: &str) -> Result<Resolved> {
    let token = release.trim();
    let major = if token.eq_ignore_ascii_case("latest") {
        let majors = fetch_majors(http).await?;
        majors
            .iter()
            .max()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no AlmaLinux majors found at {REPO_BASE}"))?
            .to_string()
    } else {
        parse_major(token)?
    };

    let images_base = format!("https://repo.almalinux.org/almalinux/{major}/cloud/x86_64/images/");
    let sums_url = format!("{images_base}CHECKSUM");
    let latest_filename = format!("AlmaLinux-{major}-GenericCloud-latest.x86_64.qcow2");

    eprintln!("Fetching {sums_url}");
    let body = http
        .get(&sums_url)
        .send()
        .await
        .with_context(|| format!("GET {sums_url}"))?
        .error_for_status()
        .with_context(|| format!("status from {sums_url}"))?
        .text()
        .await
        .with_context(|| format!("read body of {sums_url}"))?;

    let (filename, sha256) = resolve_dated_for_latest(&body, &major, &latest_filename)?;
    let build = strip_filename_chrome(&filename, &major).unwrap_or_else(|| filename.clone());
    let url = format!("{images_base}{filename}");

    Ok(Resolved {
        major,
        build,
        sha256,
        url,
    })
}

async fn fetch_majors(http: &reqwest::Client) -> Result<Vec<u32>> {
    eprintln!("Fetching AlmaLinux directory listing ...");
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
        anyhow::bail!("alma: expected a major version like '9' or 'latest', got {input:?}");
    }
    Ok(s.to_string())
}

/// Look up the sha256 of the `-latest.x86_64.qcow2` pointer in
/// CHECKSUM, then find a sibling line with the same hash that is
/// *not* the latest pointer — that's the dated form we want for
/// the manifest version.
fn resolve_dated_for_latest(
    body: &str,
    major: &str,
    latest_filename: &str,
) -> Result<(String, String)> {
    let sha256 = parse_sums(body, latest_filename)
        .ok_or_else(|| anyhow::anyhow!("CHECKSUM has no entry for {latest_filename}"))?;
    let prefix = format!("AlmaLinux-{major}-GenericCloud-");
    let dated = body
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let hash = parts.next()?.trim();
            let name = parts.next()?.trim().trim_start_matches('*');
            if hash != sha256 {
                return None;
            }
            // We want the dated build, not the rolling pointer or the
            // ext4 variant.
            if name == latest_filename {
                return None;
            }
            if !name.starts_with(&prefix) {
                return None;
            }
            if !name.ends_with(".x86_64.qcow2") {
                return None;
            }
            // Skip the `-ext4-` flavor; we want the default
            // (xfs-rooted) GenericCloud image.
            let middle = &name[prefix.len()..];
            if middle.starts_with("ext4-") {
                return None;
            }
            Some(name.to_string())
        })
        .next()
        .unwrap_or_else(|| latest_filename.to_string());
    Ok((dated, sha256))
}

/// Strip `AlmaLinux-<major>-GenericCloud-` and `.x86_64.qcow2` to
/// get the build identifier (e.g. `9.7-20260501`).
fn strip_filename_chrome(filename: &str, major: &str) -> Option<String> {
    let prefix = format!("AlmaLinux-{major}-GenericCloud-");
    let stripped = filename.strip_prefix(&prefix)?;
    let stripped = stripped.strip_suffix(".x86_64.qcow2")?;
    Some(stripped.to_string())
}

/// Linux-style `<hash>  <filename>` line lookup. Filename is matched
/// exactly. Same shape as `verify::parse_sums_file`, kept private here
/// to avoid a circular module dep.
fn parse_sums(body: &str, filename: &str) -> Option<String> {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let hash = parts.next()?.trim();
        let rest = parts.next()?.trim().trim_start_matches('*');
        if rest == filename {
            return Some(hash.to_string());
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_majors_extracts_numeric_dirs_only() {
        let body = r#"
            <a href="8/">8/</a>
            <a href="9/">9/</a>
            <a href="10/">10/</a>
            <a href="vault/">vault/</a>
            <a href="testing/">testing/</a>
        "#;
        assert_eq!(parse_majors_from_html(body), vec![8, 9, 10]);
    }

    #[test]
    fn parse_majors_dedupes_and_sorts() {
        let body = r#"href="9/" href="9/" href="8/""#;
        assert_eq!(parse_majors_from_html(body), vec![8, 9]);
    }

    #[test]
    fn parse_major_accepts_integer_only() {
        assert_eq!(parse_major("9").unwrap(), "9");
        assert_eq!(parse_major("  10  ").unwrap(), "10");
        assert!(parse_major("").is_err());
        assert!(parse_major("9.7").is_err());
        assert!(parse_major("nine").is_err());
    }

    #[test]
    fn resolve_dated_picks_dated_alias_of_latest() {
        let body = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  AlmaLinux-9-GenericCloud-9.7-20251118.x86_64.qcow2
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  AlmaLinux-9-GenericCloud-9.7-20260414.x86_64.qcow2
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  AlmaLinux-9-GenericCloud-9.7-20260501.x86_64.qcow2
dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd  AlmaLinux-9-GenericCloud-ext4-9.7-20260501.x86_64.qcow2
dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd  AlmaLinux-9-GenericCloud-ext4-latest.x86_64.qcow2
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  AlmaLinux-9-GenericCloud-latest.x86_64.qcow2
";
        let (filename, sha256) =
            resolve_dated_for_latest(body, "9", "AlmaLinux-9-GenericCloud-latest.x86_64.qcow2")
                .unwrap();
        assert_eq!(
            filename,
            "AlmaLinux-9-GenericCloud-9.7-20260501.x86_64.qcow2"
        );
        assert_eq!(sha256, "c".repeat(64));
    }

    #[test]
    fn resolve_dated_falls_back_to_latest_when_no_dated_alias() {
        let body = "\
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  AlmaLinux-9-GenericCloud-latest.x86_64.qcow2
";
        let (filename, _) =
            resolve_dated_for_latest(body, "9", "AlmaLinux-9-GenericCloud-latest.x86_64.qcow2")
                .unwrap();
        assert_eq!(filename, "AlmaLinux-9-GenericCloud-latest.x86_64.qcow2");
    }

    #[test]
    fn resolve_dated_errors_when_latest_missing() {
        let body = "aaaa  AlmaLinux-9-GenericCloud-9.7-20260501.x86_64.qcow2\n";
        assert!(
            resolve_dated_for_latest(body, "9", "AlmaLinux-9-GenericCloud-latest.x86_64.qcow2")
                .is_err()
        );
    }

    #[test]
    fn strip_filename_chrome_extracts_build() {
        assert_eq!(
            strip_filename_chrome("AlmaLinux-9-GenericCloud-9.7-20260501.x86_64.qcow2", "9"),
            Some("9.7-20260501".to_string())
        );
        assert_eq!(strip_filename_chrome("Some-Other-File.qcow2", "9"), None);
    }
}
