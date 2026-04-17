// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Minimal IMGAPI client used by `agent_install` to fetch agent
//! tarballs before handing them to APM.
//!
//! The full IMGAPI has dozens of endpoints; we need exactly two:
//!
//! * `GET /images/<uuid>` — fetch the manifest (JSON).
//! * `GET /images/<uuid>/file?storage=local` — stream the file body.
//!
//! After fetching, we verify the file's sha1 matches the manifest's
//! `files[0].sha1` and its size matches `files[0].size`. Those checks
//! mirror the legacy `getAgentImage` helper in smartos/common.js.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha1::{Digest, Sha1};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60 * 5);

#[derive(Debug, Error)]
pub enum ImgapiError {
    #[error("failed to build reqwest client: {0}")]
    BuildClient(#[source] reqwest::Error),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("status {status}: {body}")]
    Status {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("image manifest missing required field: {0}")]
    MissingField(&'static str),
    #[error("downloaded file sha1 mismatch: expected {expected}, got {actual}")]
    Sha1Mismatch { expected: String, actual: String },
    #[error("downloaded file size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Result of a successful [`ImgapiClient::fetch_agent_image`] call.
#[derive(Debug, Clone)]
pub struct AgentImage {
    /// Path to the downloaded tarball on local disk.
    pub file: PathBuf,
    /// Agent package name, extracted from the manifest's `name` field.
    pub name: String,
    /// Image manifest JSON (as returned by IMGAPI, i.e. with the
    /// `manifest` wrapping that some versions add — flatten already
    /// applied by our accessor methods).
    pub manifest: serde_json::Value,
}

/// IMGAPI HTTP client.
#[derive(Debug, Clone)]
pub struct ImgapiClient {
    base_url: String,
    http: reqwest::Client,
}

impl ImgapiClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ImgapiError> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(ImgapiError::BuildClient)?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        })
    }

    /// Fetch a manifest + file for an agent image and verify it.
    ///
    /// Writes the body to `output_dir/<output_prefix>-<file-suffix>`,
    /// where the suffix is picked based on the manifest's
    /// `files[0].compression` (`gz` / `bz2` / `none`).
    pub async fn fetch_agent_image(
        &self,
        image_uuid: &str,
        output_dir: &Path,
        output_prefix: &str,
    ) -> Result<AgentImage, ImgapiError> {
        let manifest = self.get_manifest(image_uuid).await?;

        // manifest may be wrapped as {"manifest": {...}} or bare.
        let body = manifest
            .get("manifest")
            .cloned()
            .unwrap_or_else(|| manifest.clone());

        let files = body
            .get("files")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .ok_or(ImgapiError::MissingField("files[0]"))?
            .clone();
        let expected_sha1 = files
            .get("sha1")
            .and_then(|v| v.as_str())
            .ok_or(ImgapiError::MissingField("files[0].sha1"))?
            .to_lowercase();
        let expected_size = files
            .get("size")
            .and_then(|v| v.as_u64())
            .ok_or(ImgapiError::MissingField("files[0].size"))?;
        let compression = files
            .get("compression")
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        let ext = match compression {
            "gzip" | "gz" => "tar.gz",
            "bzip2" | "bz2" => "tar.bz2",
            "none" => "tar",
            other => {
                tracing::warn!(compression = other, "unknown compression; assuming tar");
                "tar"
            }
        };
        let name = body
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or(ImgapiError::MissingField("name"))?
            .to_string();

        let file_path = output_dir.join(format!("{output_prefix}.{ext}"));
        self.download_file(image_uuid, &file_path, expected_size, &expected_sha1)
            .await?;

        Ok(AgentImage {
            file: file_path,
            name,
            manifest,
        })
    }

    async fn get_manifest(&self, image_uuid: &str) -> Result<serde_json::Value, ImgapiError> {
        let url = format!("{}/images/{image_uuid}", self.base_url);
        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ImgapiError::Status { status, body });
        }
        let manifest: serde_json::Value = resp.json().await?;
        Ok(manifest)
    }

    async fn download_file(
        &self,
        image_uuid: &str,
        path: &Path,
        expected_size: u64,
        expected_sha1: &str,
    ) -> Result<(), ImgapiError> {
        let url = format!("{}/images/{image_uuid}/file?storage=local", self.base_url);
        let mut resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ImgapiError::Status { status, body });
        }

        let mut file = tokio::fs::File::create(path)
            .await
            .map_err(|source| ImgapiError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let mut hasher = Sha1::new();
        let mut total: u64 = 0;
        while let Some(chunk) = resp.chunk().await? {
            hasher.update(&chunk);
            total = total.saturating_add(chunk.len() as u64);
            file.write_all(&chunk)
                .await
                .map_err(|source| ImgapiError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        file.flush().await.map_err(|source| ImgapiError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        if total != expected_size {
            return Err(ImgapiError::SizeMismatch {
                expected: expected_size,
                actual: total,
            });
        }
        let actual_sha1 = format!("{:x}", hasher.finalize());
        if !actual_sha1.eq_ignore_ascii_case(expected_sha1) {
            return Err(ImgapiError::Sha1Mismatch {
                expected: expected_sha1.to_string(),
                actual: actual_sha1,
            });
        }
        Ok(())
    }
}
