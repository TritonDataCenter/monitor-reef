// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Checksum verification strategies for fetched upstream images.
//!
//! `Sha256SumsTls` and `Sha512SumsTls` fetch a vendor-published
//! `<HASH>SUMS`-style listing over TLS and match by filename — same
//! threat model as a TLS-fetched URL with the hash pinned in our own
//! repo. `Sha256Pinned` is for static profiles where the caller
//! already knows the expected digest.

use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256, Sha512};
use tokio::io::AsyncReadExt;
use url::Url;

/// A `Verifier` checks the authenticity of a downloaded file. The
/// pipeline always pre-computes the file's sha256 (it needs the hash
/// for both verification and stable manifest UUID derivation), so
/// verifiers that work in sha256 use it directly. Verifiers that
/// need a different hash function (e.g. SHA-512 for Debian) get the
/// file path and hash it themselves.
#[async_trait]
pub trait Verifier: Send + Sync {
    async fn verify(
        &self,
        file: &Path,
        file_sha256_hex: &str,
        http: &reqwest::Client,
    ) -> Result<()>;
}

// Used by the Ubuntu Simple Streams path (the streams JSON gives us
// the sha256 directly, so we pin it) and by future TOML profiles.
pub struct Sha256Pinned(pub String);

#[async_trait]
impl Verifier for Sha256Pinned {
    async fn verify(
        &self,
        _file: &Path,
        file_sha256_hex: &str,
        _http: &reqwest::Client,
    ) -> Result<()> {
        if file_sha256_hex != self.0 {
            anyhow::bail!(
                "sha256 mismatch\n  expected: {}\n  actual:   {file_sha256_hex}",
                self.0
            );
        }
        eprintln!("Checksum OK: {file_sha256_hex}");
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
    async fn verify(
        &self,
        _file: &Path,
        file_sha256_hex: &str,
        http: &reqwest::Client,
    ) -> Result<()> {
        let expected =
            fetch_and_parse_sums(http, &self.sums_url, &self.filename).await?;
        if file_sha256_hex != expected {
            anyhow::bail!(
                "sha256 mismatch\n  expected: {expected} (from {})\n  actual:   {file_sha256_hex}",
                self.sums_url
            );
        }
        eprintln!("Checksum OK: {expected}");
        Ok(())
    }
}

pub struct Sha512SumsTls {
    pub sums_url: Url,
    pub filename: String,
}

impl Sha512SumsTls {
    pub fn new(sums_url: Url, filename: String) -> Self {
        Self { sums_url, filename }
    }
}

#[async_trait]
impl Verifier for Sha512SumsTls {
    async fn verify(
        &self,
        file: &Path,
        _file_sha256_hex: &str,
        http: &reqwest::Client,
    ) -> Result<()> {
        let expected =
            fetch_and_parse_sums(http, &self.sums_url, &self.filename).await?;
        let actual = sha512_file(file).await?;
        if actual != expected {
            anyhow::bail!(
                "sha512 mismatch\n  expected: {expected} (from {})\n  actual:   {actual}",
                self.sums_url
            );
        }
        eprintln!("Checksum OK (sha512): {expected}");
        Ok(())
    }
}

/// Some vendors (Alpine) publish a per-image sidecar URL that is just
/// the bare hash on a single line — no filename, no comment. Different
/// shape from a `<HASH>SUMS` file but same threat model.
pub struct Sha512SidecarTls {
    pub sidecar_url: Url,
}

#[async_trait]
impl Verifier for Sha512SidecarTls {
    async fn verify(
        &self,
        file: &Path,
        _file_sha256_hex: &str,
        http: &reqwest::Client,
    ) -> Result<()> {
        eprintln!("Fetching {}", self.sidecar_url);
        let body = http
            .get(self.sidecar_url.clone())
            .send()
            .await
            .with_context(|| format!("GET {}", self.sidecar_url))?
            .error_for_status()
            .with_context(|| format!("status from {}", self.sidecar_url))?
            .text()
            .await
            .with_context(|| format!("read body of {}", self.sidecar_url))?;
        let expected = body
            .split_whitespace()
            .next()
            .ok_or_else(|| anyhow::anyhow!("empty sha512 sidecar at {}", self.sidecar_url))?
            .to_string();
        let actual = sha512_file(file).await?;
        if actual != expected {
            anyhow::bail!(
                "sha512 mismatch\n  expected: {expected} (from {})\n  actual:   {actual}",
                self.sidecar_url
            );
        }
        eprintln!("Checksum OK (sha512): {expected}");
        Ok(())
    }
}

async fn fetch_and_parse_sums(
    http: &reqwest::Client,
    sums_url: &Url,
    filename: &str,
) -> Result<String> {
    eprintln!("Fetching {sums_url}");
    let body = http
        .get(sums_url.clone())
        .send()
        .await
        .with_context(|| format!("GET {sums_url}"))?
        .error_for_status()
        .with_context(|| format!("status from {sums_url}"))?
        .text()
        .await
        .with_context(|| format!("read body of {sums_url}"))?;
    parse_sums_file(&body, filename)
        .ok_or_else(|| anyhow::anyhow!("filename {filename} not found in {sums_url}"))
}

/// Parse a `<HASH>SUMS`-style listing. Each non-empty, non-comment line
/// is `<hex>  [*]<filename>`. The asterisk prefix means binary mode and
/// is stripped; whitespace-as-separator handles single-space or
/// multi-space delimiters. The hash function isn't validated here —
/// callers tell upstream which hash they're expecting via the URL.
fn parse_sums_file(body: &str, filename: &str) -> Option<String> {
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
    hash_file::<Sha256>(file).await
}

pub async fn sha512_file(file: &Path) -> Result<String> {
    hash_file::<Sha512>(file).await
}

async fn hash_file<H: Digest>(file: &Path) -> Result<String> {
    let mut f = tokio::fs::File::open(file)
        .await
        .with_context(|| format!("open {}", file.display()))?;
    let mut hasher = H::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format_hex(&hasher.finalize()))
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
    fn parse_sums_file_canonical_lines() {
        let body = "\
abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234 *foo.img\n\
deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef  bar.img\n\
# comment line\n\
\n\
0000000000000000000000000000000000000000000000000000000000000000  baz.img\n";
        assert_eq!(
            parse_sums_file(body, "foo.img"),
            Some("abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234".to_string())
        );
        assert_eq!(
            parse_sums_file(body, "bar.img"),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string())
        );
        assert_eq!(parse_sums_file(body, "missing.img"), None);
    }

    #[test]
    fn parse_sums_file_works_for_sha512_lines() {
        // Hash length isn't validated; whatever hex appears in the
        // first column is returned as-is. SHA-512 produces 128 hex
        // chars vs SHA-256's 64.
        let body = "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000  debian-13-genericcloud-amd64.qcow2\n";
        assert_eq!(
            parse_sums_file(body, "debian-13-genericcloud-amd64.qcow2"),
            Some("00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000".to_string())
        );
    }
}
