// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! S3-backed blob storage for IMGAPI image content.
//!
//! tritond's IMGAPI surface stores the small structured manifest in
//! FDB; this crate handles the large binary payload (the gzipped
//! `zfs send` stream), which lives in an S3 bucket. The bucket is
//! conventionally `triton-images` on an in-cluster mantad zone, but
//! any S3-compatible endpoint (AWS, MinIO, ceph-rgw, etc.) works.
//!
//! ## Layout
//!
//! Object key for an image is `<uuid>/file`. The full URL is
//! `<endpoint>/<bucket>/<uuid>/file` (path-style).
//!
//! ## Auth split
//!
//! - **Uploads** (`BlobStore::upload`, `delete`) use SigV4 signing
//!   via the [`rusty_s3`] crate. The caller supplies an access key
//!   and a secret key at construction time; mantad's root credentials
//!   (auto-minted at first boot, persisted in
//!   `/data/etc/mantad/secrets.env`) are the typical source.
//! - **Reads** are plain anonymous HTTPS GET against the public URL
//!   returned by [`BlobStore::url_for`]. Per-CN agents do not need
//!   credentials. This works because the bucket policy on
//!   `triton-images` allows `Principal: "*"` for `s3:GetObject`; an
//!   operator runs `aws s3api put-bucket-policy` once at deploy
//!   time. See `images/triton-mantad/README.md` for the policy JSON.
//!
//! ## Integrity
//!
//! SHA-1 is the IMGAPI wire requirement for `files[].sha1`.
//! [`BlobStore::upload`] streams the source file through a SHA-1
//! hasher before invoking the signed PUT, returning the digest and
//! size so the caller can populate the manifest before POSTing it
//! to tritond.

use std::path::Path;
use std::time::Duration;

use rusty_s3::actions::{DeleteObject, PutObject, S3Action};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use sha1::{Digest, Sha1};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tracing::info;
use url::Url;
use uuid::Uuid;

/// How long a SigV4 presigned URL stays valid. We never store these
/// — they are minted, used immediately, and discarded inside a
/// single upload call — so the duration is just a comfortable
/// upper bound on the upload latency.
const PRESIGN_TTL: Duration = Duration::from_secs(900);

/// Errors from blob upload, delete, or hash operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BlobError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid endpoint url: {0}")]
    Url(#[from] url::ParseError),
    #[error("invalid bucket name: {0}")]
    BadBucket(String),
    #[error("invalid endpoint: {0}")]
    BadEndpoint(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("S3 PUT {url} returned {status}: {body}")]
    PutFailed {
        url: String,
        status: u16,
        body: String,
    },
    #[error("S3 DELETE {url} returned {status}: {body}")]
    DeleteFailed {
        url: String,
        status: u16,
        body: String,
    },
}

/// Result of a successful blob upload. Caller stamps these onto the
/// IMGAPI manifest's `files[0]` before POSTing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadOutcome {
    /// Lowercase 40-char hex SHA-1 of the uploaded bytes.
    pub sha1: String,
    /// Size of the uploaded bytes.
    pub size: u64,
    /// Public HTTPS URL the bytes are reachable from (no auth needed).
    pub public_url: String,
}

/// Configuration for an S3-backed blob store.
///
/// One [`BlobStore`] instance is held by tritond's HTTP context (or
/// instantiated per-invocation by tritonadm's upload verb).
#[derive(Debug, Clone)]
pub struct BlobStore {
    bucket: Bucket,
    credentials: Credentials,
    client: reqwest::Client,
}

impl BlobStore {
    /// Construct a new blob store.
    ///
    /// `endpoint` is the base URL of the S3 service
    /// (e.g. `http://172.16.96.6:7443`). `region` is the SigV4
    /// region label (e.g. `us-east-1`); mantad accepts any region as
    /// long as the SigV4 calculation matches. `bucket` must already
    /// exist with a public-read policy applied; this crate does not
    /// manage bucket lifecycle.
    pub fn new(
        endpoint: Url,
        region: impl Into<String>,
        bucket: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Result<Self, BlobError> {
        let bucket_name = bucket.into();
        let bucket = Bucket::new(endpoint, UrlStyle::Path, bucket_name, region.into())
            .map_err(|e| BlobError::BadBucket(format!("{e}")))?;
        let credentials = Credentials::new(access_key, secret_key);

        // reqwest 0.13's `rustls` feature uses rustls-platform-verifier,
        // which scans the system trust store and fails on illumos with
        // "No CA certificates were loaded from the system". Bundle the
        // Mozilla webpki roots so TLS works the same on every platform.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .build()
            .map_err(BlobError::Http)?;
        Ok(Self {
            bucket,
            credentials,
            client,
        })
    }

    /// Build the public anonymous HTTPS GET URL for `uuid`'s blob.
    /// Caller stores this on the IMGAPI manifest record as the
    /// agent-side fetch URL.
    pub fn url_for(&self, uuid: Uuid) -> String {
        // rusty-s3's Bucket::base_url() with UrlStyle::Path already
        // includes the bucket name in the path
        // (`<endpoint>/<bucket>/`), so we just append the key.
        let base = self.bucket.base_url().as_str().trim_end_matches('/');
        format!("{base}/{uuid}/file")
    }

    /// S3 object key under the bucket for `uuid`'s blob.
    pub fn key_for(&self, uuid: Uuid) -> String {
        format!("{uuid}/file")
    }

    /// SHA-1 the local file, then SigV4-PUT it to `<bucket>/<uuid>/file`.
    /// Returns the digest, size, and public URL the caller should
    /// stamp on the IMGAPI manifest.
    ///
    /// The local file is read twice: once streamed through a SHA-1
    /// hasher (to compute the digest before the upload begins, so a
    /// failed upload leaves a known-good local copy), once by
    /// reqwest as the PUT body.
    pub async fn upload(&self, uuid: Uuid, local_path: &Path) -> Result<UploadOutcome, BlobError> {
        let (sha1, size) = hash_file_sha1(local_path).await?;
        let key = self.key_for(uuid);
        let public_url = self.url_for(uuid);

        let put = PutObject::new(&self.bucket, Some(&self.credentials), &key);
        let signed_url = put.sign(PRESIGN_TTL);

        info!(
            uuid = %uuid,
            local = %local_path.display(),
            size,
            sha1 = %sha1,
            "S3 PUT"
        );

        let file = tokio::fs::File::open(local_path).await?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let resp = self
            .client
            .put(signed_url.clone())
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BlobError::PutFailed {
                url: signed_url.to_string(),
                status: status.as_u16(),
                body,
            });
        }

        Ok(UploadOutcome {
            sha1,
            size,
            public_url,
        })
    }

    /// SigV4-DELETE the blob for `uuid`. Idempotent: a delete of a
    /// non-existent object returns 204 from S3.
    pub async fn delete(&self, uuid: Uuid) -> Result<(), BlobError> {
        let key = self.key_for(uuid);
        let del = DeleteObject::new(&self.bucket, Some(&self.credentials), &key);
        let signed_url = del.sign(PRESIGN_TTL);

        info!(uuid = %uuid, key = %key, "S3 DELETE");

        let resp = self.client.delete(signed_url.clone()).send().await?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 404 {
            let body = resp.text().await.unwrap_or_default();
            return Err(BlobError::DeleteFailed {
                url: signed_url.to_string(),
                status: status.as_u16(),
                body,
            });
        }
        Ok(())
    }
}

/// Stream `path` through a SHA-1 hasher. Returns the lowercase hex
/// digest and total size. Used by [`BlobStore::upload`] and reusable
/// for caller-side verification of an already-uploaded local file.
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

    fn store() -> BlobStore {
        BlobStore::new(
            Url::parse("http://172.16.96.6:7443").unwrap(),
            "us-east-1",
            "triton-images",
            "AKIATEST",
            "secretdontuse",
        )
        .unwrap()
    }

    #[test]
    fn url_for_path_style() {
        let s = store();
        let uuid = Uuid::parse_str("c02a2044-c1bd-11e4-bd8c-dfc1db8b0182").unwrap();
        assert_eq!(
            s.url_for(uuid),
            "http://172.16.96.6:7443/triton-images/c02a2044-c1bd-11e4-bd8c-dfc1db8b0182/file"
        );
    }

    #[test]
    fn key_for_uuid() {
        let s = store();
        let uuid = Uuid::parse_str("c02a2044-c1bd-11e4-bd8c-dfc1db8b0182").unwrap();
        assert_eq!(
            s.key_for(uuid),
            "c02a2044-c1bd-11e4-bd8c-dfc1db8b0182/file"
        );
    }

    // Note: rusty-s3 0.7 is permissive about bucket names (it
    // accepts uppercase, empty, and other forms that strict S3
    // would reject). Bucket-name validation is the caller's
    // responsibility; we surface whatever the upstream constructor
    // returns via the BadBucket arm if it ever does start failing.

    #[tokio::test]
    async fn hash_file_sha1_matches_known_input() {
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
        assert_eq!(sha, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(size, 0);
    }

    /// Round-trip smoke test against a real S3 endpoint.
    /// Gated behind IMGAPI_BLOB_E2E so it only runs when an operator
    /// has a reachable mantad and explicitly sets the env. The four
    /// env vars mirror the fields on BlobStore::new.
    #[tokio::test]
    async fn e2e_put_and_anonymous_get() {
        if std::env::var("IMGAPI_BLOB_E2E").is_err() {
            eprintln!("skipping e2e test (set IMGAPI_BLOB_E2E=1 to run)");
            return;
        }
        let endpoint = std::env::var("IMGAPI_BLOB_E2E_ENDPOINT")
            .expect("IMGAPI_BLOB_E2E_ENDPOINT (e.g. http://localhost:7443)");
        let region =
            std::env::var("IMGAPI_BLOB_E2E_REGION").unwrap_or_else(|_| "us-east-1".to_string());
        let bucket = std::env::var("IMGAPI_BLOB_E2E_BUCKET")
            .unwrap_or_else(|_| "triton-images".to_string());
        let ak = std::env::var("IMGAPI_BLOB_E2E_AK").expect("IMGAPI_BLOB_E2E_AK");
        let sk = std::env::var("IMGAPI_BLOB_E2E_SK").expect("IMGAPI_BLOB_E2E_SK");

        let store = BlobStore::new(Url::parse(&endpoint).unwrap(), region, bucket, ak, sk).unwrap();
        let uuid = Uuid::new_v4();

        let dir = tempfile::tempdir().unwrap();
        let body = b"e2e-blob-payload\n";
        let path = dir.path().join("payload.bin");
        tokio::fs::write(&path, body).await.unwrap();

        let outcome = store.upload(uuid, &path).await.expect("upload");
        assert_eq!(outcome.size, body.len() as u64);
        eprintln!("uploaded {uuid} -> {}", outcome.public_url);

        // Anonymous GET: no auth headers; succeeds only if the
        // bucket policy permits anonymous read.
        let resp = reqwest::get(&outcome.public_url).await.expect("anon GET");
        assert!(
            resp.status().is_success(),
            "anonymous GET returned {}",
            resp.status()
        );
        let fetched = resp.bytes().await.unwrap();
        assert_eq!(&fetched[..], body);

        store.delete(uuid).await.expect("delete");
    }
}
