// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Load a TOML profile into a `ResolvedImage`.
//!
//! TOML profiles skip release discovery — the file *is* the resolved
//! tuple. The schema is documented under `docs/design/examples/nocloud-vendors/`
//! and the design doc's "External: TOML profiles for pinned URLs"
//! section. The only verifier strategy a TOML can express is
//! `Sha256Pinned`; anything else would require dragging release-
//! discovery logic back in, which defeats the point.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use url::Url;

use super::{ResolvedImage, SourceFormat};
use crate::commands::image::nocloud::verify::Sha256Pinned;

/// On-disk schema. Field names match the design doc's table; serde
/// rejects unknown fields so a typo doesn't silently get ignored.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    url: String,
    format: SourceFormat,
    os: String,
    series: String,
    version: String,
    sha256: String,
    description: String,
    homepage: String,
    ssh_key: bool,
}

/// Load a TOML profile, returning a `(vendor_label, ResolvedImage)`
/// pair. The vendor label is the file's stem and is used in
/// user-facing messages and the default workdir/output paths in
/// place of the built-in `Vendor` enum string.
pub async fn load(path: &Path) -> Result<(String, ResolvedImage)> {
    let body = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read TOML profile {}", path.display()))?;
    let profile: Profile =
        toml::from_str(&body).with_context(|| format!("parse TOML profile {}", path.display()))?;
    let vendor_label = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("TOML profile {} has no usable file stem", path.display()))?
        .to_string();
    let resolved = profile
        .into_resolved()
        .with_context(|| format!("validate TOML profile {}", path.display()))?;
    Ok((vendor_label, resolved))
}

impl Profile {
    fn into_resolved(self) -> Result<ResolvedImage> {
        let url: Url = self
            .url
            .parse()
            .with_context(|| format!("invalid url {:?}", self.url))?;
        let homepage: Url = self
            .homepage
            .parse()
            .with_context(|| format!("invalid homepage {:?}", self.homepage))?;
        let sha256 = normalize_sha256(&self.sha256)?;
        Ok(ResolvedImage {
            url,
            format: self.format,
            os: self.os,
            series: self.series,
            version: self.version,
            description: self.description,
            homepage,
            ssh_key: self.ssh_key,
            verifier: Box::new(Sha256Pinned(sha256.clone())),
            expected_sha256: Some(sha256),
        })
    }
}

/// Lowercase and validate a 64-hex-char sha256. Operators commonly
/// paste the hash from upstream sums files which are conventionally
/// lowercase, but accept any case so a hand-typed entry doesn't
/// fail awkwardly.
fn normalize_sha256(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!(
            "sha256 must be exactly 64 hex chars, got {} chars: {:?}",
            trimmed.len(),
            trimmed
        );
    }
    Ok(trimmed.to_ascii_lowercase())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    async fn write_tmp(name: &str, body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join(name), body).await.unwrap();
        dir
    }

    const ALMA_GOLDEN: &str = r#"
url         = "https://vault.almalinux.org/9.4/cloud/x86_64/images/AlmaLinux-9-GenericCloud-9.4-20240805.x86_64.qcow2"
format      = "qcow2"
os          = "linux"
series      = "alma9"
version     = "9.4-20240805"
sha256      = "4f2984589020c0d82b9a410cf9e29715a607c948dfdca652025cdc79ddb5e816"
description = "AlmaLinux 9.4 GenericCloud (vaulted)."
homepage    = "https://almalinux.org/"
ssh_key     = true
"#;

    #[tokio::test]
    async fn load_alma_golden_round_trips() {
        let dir = write_tmp("alma-9.4-pinned.toml", ALMA_GOLDEN).await;
        let (label, resolved) = load(&dir.path().join("alma-9.4-pinned.toml"))
            .await
            .unwrap();

        assert_eq!(label, "alma-9.4-pinned");
        assert_eq!(resolved.url.host_str(), Some("vault.almalinux.org"));
        assert!(matches!(resolved.format, SourceFormat::Qcow2));
        assert_eq!(resolved.os, "linux");
        assert_eq!(resolved.series, "alma9");
        assert_eq!(resolved.version, "9.4-20240805");
        assert_eq!(resolved.homepage.as_str(), "https://almalinux.org/");
        assert!(resolved.ssh_key);
        assert_eq!(
            resolved.expected_sha256.as_deref(),
            Some("4f2984589020c0d82b9a410cf9e29715a607c948dfdca652025cdc79ddb5e816")
        );
    }

    #[tokio::test]
    async fn load_file_url_for_local_image() {
        let body = r#"
url         = "file:///var/tmp/staging/fossil.qcow2"
format      = "qcow2"
os          = "plan9"
series      = "plan9-fossil"
version     = "stanleylieber-snapshot"
sha256      = "0000000000000000000000000000000000000000000000000000000000000000"
description = "Plan 9 fossil install."
homepage    = "https://9p.io/"
ssh_key     = false
"#;
        let dir = write_tmp("plan9-fossil.toml", body).await;
        let (label, resolved) = load(&dir.path().join("plan9-fossil.toml")).await.unwrap();
        assert_eq!(label, "plan9-fossil");
        assert_eq!(resolved.url.scheme(), "file");
        assert_eq!(resolved.os, "plan9");
        assert!(!resolved.ssh_key);
    }

    #[tokio::test]
    async fn load_raw_format_parses() {
        let body = r#"
url         = "file:///var/tmp/9legacy.img"
format      = "raw"
os          = "plan9"
series      = "plan9-9legacy"
version     = "2019-04-21"
sha256      = "0000000000000000000000000000000000000000000000000000000000000000"
description = "Plan 9 9legacy."
homepage    = "http://9legacy.org/"
ssh_key     = false
"#;
        let dir = write_tmp("plan9-9legacy.toml", body).await;
        let (_, resolved) = load(&dir.path().join("plan9-9legacy.toml")).await.unwrap();
        assert!(matches!(resolved.format, SourceFormat::Raw));
    }

    #[tokio::test]
    async fn rejects_unknown_format() {
        let body = ALMA_GOLDEN.replace("\"qcow2\"", "\"bz2\"");
        let dir = write_tmp("bad.toml", &body).await;
        let err = format!(
            "{:#}",
            load(&dir.path().join("bad.toml")).await.err().unwrap()
        );
        assert!(err.contains("parse TOML profile"), "{err}");
    }

    #[tokio::test]
    async fn rejects_short_sha256() {
        let body = ALMA_GOLDEN.replace(
            "\"4f2984589020c0d82b9a410cf9e29715a607c948dfdca652025cdc79ddb5e816\"",
            "\"deadbeef\"",
        );
        let dir = write_tmp("bad.toml", &body).await;
        let err = format!(
            "{:#}",
            load(&dir.path().join("bad.toml")).await.err().unwrap()
        );
        assert!(err.contains("sha256"), "{err}");
    }

    #[tokio::test]
    async fn rejects_non_hex_sha256() {
        let body = ALMA_GOLDEN.replace(
            "4f2984589020c0d82b9a410cf9e29715a607c948dfdca652025cdc79ddb5e816",
            "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
        );
        let dir = write_tmp("bad.toml", &body).await;
        let err = format!(
            "{:#}",
            load(&dir.path().join("bad.toml")).await.err().unwrap()
        );
        assert!(err.contains("sha256"), "{err}");
    }

    #[tokio::test]
    async fn accepts_uppercase_sha256_and_lowercases_it() {
        let body = ALMA_GOLDEN.replace(
            "4f2984589020c0d82b9a410cf9e29715a607c948dfdca652025cdc79ddb5e816",
            "4F2984589020C0D82B9A410CF9E29715A607C948DFDCA652025CDC79DDB5E816",
        );
        let dir = write_tmp("upper.toml", &body).await;
        let (_, resolved) = load(&dir.path().join("upper.toml")).await.unwrap();
        assert_eq!(
            resolved.expected_sha256.as_deref(),
            Some("4f2984589020c0d82b9a410cf9e29715a607c948dfdca652025cdc79ddb5e816")
        );
    }

    #[tokio::test]
    async fn rejects_unknown_field() {
        let body = format!("{ALMA_GOLDEN}\nextra = \"oops\"\n");
        let dir = write_tmp("extra.toml", &body).await;
        let err = format!(
            "{:#}",
            load(&dir.path().join("extra.toml")).await.err().unwrap()
        );
        assert!(err.contains("parse TOML profile"), "{err}");
    }

    #[tokio::test]
    async fn rejects_missing_required_field() {
        // Drop the `series` line.
        let body: String = ALMA_GOLDEN
            .lines()
            .filter(|l| !l.trim_start().starts_with("series"))
            .collect::<Vec<_>>()
            .join("\n");
        let dir = write_tmp("missing.toml", &body).await;
        let err = format!(
            "{:#}",
            load(&dir.path().join("missing.toml")).await.err().unwrap()
        );
        assert!(err.contains("parse TOML profile"), "{err}");
    }

    #[tokio::test]
    async fn rejects_invalid_url() {
        let body = ALMA_GOLDEN.replace(
            "\"https://vault.almalinux.org/9.4/cloud/x86_64/images/AlmaLinux-9-GenericCloud-9.4-20240805.x86_64.qcow2\"",
            "\"not-a-url\"",
        );
        let dir = write_tmp("badurl.toml", &body).await;
        let err = format!(
            "{:#}",
            load(&dir.path().join("badurl.toml")).await.err().unwrap()
        );
        assert!(
            err.contains("invalid url") || err.contains("validate"),
            "{err}"
        );
    }
}
