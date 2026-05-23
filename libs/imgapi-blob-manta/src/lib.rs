// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Manta-backed blob storage for IMGAPI image content.
//!
//! tritond's IMGAPI surface stores the small structured
//! manifest in FDB; this crate handles the large binary
//! payload (`files[].compression`-wrapped `zfs send` stream),
//! which lives in Manta.
//!
//! ## Layout
//!
//! All blobs land under a single configured path prefix:
//!
//! ```text
//! <manta_path_prefix>/<image-uuid>/file
//! ```
//!
//! The same uuid drives the public HTTPS URL:
//!
//! ```text
//! <manta_url_prefix>/<image-uuid>/file
//! ```
//!
//! Compression suffix is intentionally absent from the path.
//! The manifest's `files[0].compression` tells the agent how
//! to decode the bytes; the path is stable across compression
//! migrations of the same image identity.
//!
//! ## Auth model
//!
//! Uploads shell out to `mput` (node-manta), which already
//! handles SSH-agent signing, HMAC, large-file streaming, and
//! the standard Manta env vars (`MANTA_URL`, `MANTA_USER`,
//! `MANTA_KEY_ID`). Mirrors the pattern used by
//! `cli/tritoncloud-publish/src/manta.rs`.
//!
//! Downloads are plain HTTPS GET against the public URL. The
//! crate does not need Manta credentials at read time; the
//! per-CN agent only needs a working HTTPS stack with our
//! trust roots.
//!
//! Tritond and tritonadm both use this crate. Tritond resolves
//! a blob URL when serving `GET /images/:uuid/file`; tritonadm
//! uploads bytes when running `image upload`.
//!
//! ## Integrity
//!
//! SHA-1 is the IMGAPI wire requirement for `files[].sha1`.
//! [`BlobStore::upload`] streams the source file through a
//! SHA-1 hasher before invoking `mput`, returning the digest
//! and size so the caller can populate the manifest before
//! POSTing it to tritond.

use std::path::Path;
use std::process::Stdio;

use sha1::{Digest, Sha1};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::info;
use uuid::Uuid;

/// Errors from blob upload, head, or delete operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BlobError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("mput {remote_path} exited with status {status}")]
    MputFailed { remote_path: String, status: i32 },
    #[error("mrm {remote_path} exited with status {status}")]
    MrmFailed { remote_path: String, status: i32 },
    #[error("path prefix must be absolute Manta path starting with '/', got {got:?}")]
    BadPathPrefix { got: String },
    #[error("url prefix must be absolute https:// URL, got {got:?}")]
    BadUrlPrefix { got: String },
}

/// Result of a successful blob upload. Caller stamps these
/// onto the IMGAPI manifest's `files[0]` before POSTing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadOutcome {
    /// Lowercase 40-char hex SHA-1 of the uploaded bytes.
    pub sha1: String,
    /// Size of the uploaded bytes.
    pub size: u64,
    /// Full Manta object path the bytes landed at.
    pub manta_path: String,
    /// Public HTTPS URL the bytes are reachable from.
    pub public_url: String,
}

/// Configuration for a Manta-backed blob store.
///
/// One [`BlobStore`] instance is shared across tritond's
/// request handlers (held in app state) or instantiated
/// per-invocation by tritonadm.
#[derive(Debug, Clone)]
pub struct BlobStore {
    /// Manta object-path prefix, e.g.
    /// `/nick.wilkens@mnxsolutions.com/public/imgapi`. Used
    /// in `mput`/`mrm` shell-outs.
    manta_path_prefix: String,
    /// Public HTTPS URL prefix that resolves to
    /// `manta_path_prefix`, e.g.
    /// `https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/imgapi`.
    manta_url_prefix: String,
}

impl BlobStore {
    /// Construct a new blob store. Both prefixes are stored
    /// verbatim modulo trailing-slash normalization; trailing
    /// `/` is stripped so [`Self::manta_path_for`] and
    /// [`Self::url_for`] produce stable concatenations.
    pub fn new(manta_path_prefix: &str, manta_url_prefix: &str) -> Result<Self, BlobError> {
        if !manta_path_prefix.starts_with('/') {
            return Err(BlobError::BadPathPrefix {
                got: manta_path_prefix.to_string(),
            });
        }
        if !(manta_url_prefix.starts_with("https://") || manta_url_prefix.starts_with("http://")) {
            return Err(BlobError::BadUrlPrefix {
                got: manta_url_prefix.to_string(),
            });
        }
        Ok(Self {
            manta_path_prefix: trim_slash(manta_path_prefix),
            manta_url_prefix: trim_slash(manta_url_prefix),
        })
    }

    /// Manta object path for `uuid`'s blob.
    pub fn manta_path_for(&self, uuid: Uuid) -> String {
        format!("{}/{uuid}/file", self.manta_path_prefix)
    }

    /// Public HTTPS URL for `uuid`'s blob.
    pub fn url_for(&self, uuid: Uuid) -> String {
        format!("{}/{uuid}/file", self.manta_url_prefix)
    }

    /// Hash + mput the file at `local_path` to the canonical
    /// Manta path for `uuid`. The local file is read twice: once
    /// streamed through a SHA-1 hasher (to compute the digest
    /// before the upload starts, so a failed mput leaves a
    /// known-good local image), once by `mput` itself.
    pub async fn upload(&self, uuid: Uuid, local_path: &Path) -> Result<UploadOutcome, BlobError> {
        let (sha1, size) = hash_file_sha1(local_path).await?;
        let manta_path = self.manta_path_for(uuid);
        let public_url = self.url_for(uuid);
        info!(
            uuid = %uuid,
            local = %local_path.display(),
            remote = %manta_path,
            size,
            "mput blob"
        );
        let status = Command::new("mput")
            .arg("-f")
            .arg(local_path)
            .arg(&manta_path)
            .stdin(Stdio::null())
            .status()
            .await?;
        if !status.success() {
            return Err(BlobError::MputFailed {
                remote_path: manta_path,
                status: status.code().unwrap_or(-1),
            });
        }
        Ok(UploadOutcome {
            sha1,
            size,
            manta_path,
            public_url,
        })
    }

    /// Remove the blob for `uuid`. Idempotent: `mrm` of a
    /// non-existent object returns success on Manta.
    pub async fn delete(&self, uuid: Uuid) -> Result<(), BlobError> {
        let manta_path = self.manta_path_for(uuid);
        info!(uuid = %uuid, remote = %manta_path, "mrm blob");
        let status = Command::new("mrm")
            .arg(&manta_path)
            .stdin(Stdio::null())
            .status()
            .await?;
        if !status.success() {
            return Err(BlobError::MrmFailed {
                remote_path: manta_path,
                status: status.code().unwrap_or(-1),
            });
        }
        Ok(())
    }
}

fn trim_slash(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

/// Stream `path` through a SHA-1 hasher. Returns the lowercase
/// hex digest and total size. Used by [`BlobStore::upload`] and
/// reusable for caller-side verification of an already-uploaded
/// local file.
pub async fn hash_file_sha1(path: &Path) -> Result<(String, u64), BlobError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let digest = hex_lower(&hasher.finalize());
    Ok((digest, total))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_path_prefix() {
        let err = BlobStore::new("public/imgapi", "https://example.com").expect_err("must reject");
        assert!(matches!(err, BlobError::BadPathPrefix { .. }));
    }

    #[test]
    fn rejects_non_http_url_prefix() {
        let err = BlobStore::new("/u/public/imgapi", "manta:///foo").expect_err("must reject");
        assert!(matches!(err, BlobError::BadUrlPrefix { .. }));
    }

    #[test]
    fn url_and_path_construction_is_stable() {
        let store = BlobStore::new(
            "/nick.wilkens@mnxsolutions.com/public/imgapi/",
            "https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/imgapi/",
        )
        .unwrap();
        let uuid = Uuid::parse_str("c02a2044-c1bd-11e4-bd8c-dfc1db8b0182").unwrap();
        assert_eq!(
            store.manta_path_for(uuid),
            "/nick.wilkens@mnxsolutions.com/public/imgapi/c02a2044-c1bd-11e4-bd8c-dfc1db8b0182/file"
        );
        assert_eq!(
            store.url_for(uuid),
            "https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/imgapi/c02a2044-c1bd-11e4-bd8c-dfc1db8b0182/file"
        );
    }

    #[tokio::test]
    async fn hash_file_sha1_matches_known_input() {
        // SHA-1 of "hello world\n" is
        // 22596363b3de40b06f981fb85d82312e8c0ed511 — well-known.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        tokio::fs::write(&path, b"hello world\n").await.unwrap();
        let (sha, size) = hash_file_sha1(&path).await.unwrap();
        assert_eq!(sha, "22596363b3de40b06f981fb85d82312e8c0ed511");
        assert_eq!(size, 12);
    }

    #[tokio::test]
    async fn hash_file_sha1_handles_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        tokio::fs::write(&path, b"").await.unwrap();
        let (sha, size) = hash_file_sha1(&path).await.unwrap();
        // SHA-1 of empty input.
        assert_eq!(sha, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(size, 0);
    }
}
