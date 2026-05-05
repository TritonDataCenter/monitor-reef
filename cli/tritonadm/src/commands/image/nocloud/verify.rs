// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Checksum verification strategies for fetched upstream images.
//!
//! `Sha256SumsTls` fetches a vendor-published `SHA256SUMS`-style listing
//! over TLS and matches by filename. This is the POC trust floor — same
//! threat model as a TLS-fetched URL with a hash pinned in our own repo.
//! `Sha256Pinned` is for static profiles where the caller already knows
//! the expected digest.

use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use url::Url;

#[async_trait]
pub trait Verifier: Send + Sync {
    async fn verify(&self, file: &Path, http: &reqwest::Client) -> Result<()>;
}

// Sha256Pinned covers the static-conf style (single URL + literal hash)
// used by .conf files in target/triton-nocloud-images. Wired into TOML
// profile loading in follow-up work; kept here so the trait surface is
// stable across vendor additions.
#[allow(dead_code)]
pub struct Sha256Pinned(pub String);

#[async_trait]
impl Verifier for Sha256Pinned {
    async fn verify(&self, file: &Path, _http: &reqwest::Client) -> Result<()> {
        let actual = sha256_file(file).await?;
        if actual != self.0 {
            anyhow::bail!(
                "sha256 mismatch for {}\n  expected: {}\n  actual:   {}",
                file.display(),
                self.0,
                actual
            );
        }
        Ok(())
    }
}

pub struct Sha256SumsTls {
    pub sums_url: Url,
    pub filename: String,
}

impl Sha256SumsTls {
    pub fn new(sums_url: Url, filename: String) -> Self {
        Self { sums_url, filename }
    }
}

#[async_trait]
impl Verifier for Sha256SumsTls {
    async fn verify(&self, file: &Path, http: &reqwest::Client) -> Result<()> {
        eprintln!("Fetching {}", self.sums_url);
        let body = http
            .get(self.sums_url.clone())
            .send()
            .await
            .with_context(|| format!("GET {}", self.sums_url))?
            .error_for_status()
            .with_context(|| format!("status from {}", self.sums_url))?
            .text()
            .await
            .with_context(|| format!("read body of {}", self.sums_url))?;

        let expected = parse_sha256sums(&body, &self.filename).ok_or_else(|| {
            anyhow::anyhow!(
                "filename {} not found in {}",
                self.filename,
                self.sums_url
            )
        })?;

        let actual = sha256_file(file).await?;
        if actual != expected {
            anyhow::bail!(
                "sha256 mismatch for {}\n  expected: {} (from {})\n  actual:   {}",
                file.display(),
                expected,
                self.sums_url,
                actual
            );
        }
        eprintln!("Checksum OK: {}", expected);
        Ok(())
    }
}

/// Parse a `SHA256SUMS`-style listing. Each non-empty, non-comment line is
/// `<hex>  [*]<filename>`. The asterisk prefix means binary mode and is
/// stripped; whitespace-as-separator handles either single-space or
/// multi-space delimiters.
fn parse_sha256sums(body: &str, filename: &str) -> Option<String> {
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

pub async fn sha256_file(file: &Path) -> Result<String> {
    let mut f = tokio::fs::File::open(file)
        .await
        .with_context(|| format!("open {}", file.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(format_hex(&digest))
}

fn format_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sha256sums_canonical_lines() {
        let body = "\
abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234 *foo.img\n\
deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef  bar.img\n\
# comment line\n\
\n\
0000000000000000000000000000000000000000000000000000000000000000  baz.img\n";
        assert_eq!(
            parse_sha256sums(body, "foo.img"),
            Some("abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_string())
        );
        assert_eq!(
            parse_sha256sums(body, "bar.img"),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string())
        );
        assert_eq!(parse_sha256sums(body, "missing.img"), None);
    }
}
