// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image file and compression types for IMGAPI

use super::common::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// File compression type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileCompression {
    Gzip,
    Bzip2,
    Xz,
    None,
    /// Catch-all for compression types added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// Storage backend type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageType {
    Local,
    Manta,
    /// Catch-all for storage types added after this client was compiled
    #[serde(other)]
    Unknown,
}

/// Image file metadata
///
/// Represents a single file artifact associated with an image.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageFile {
    /// SHA-1 hash of the file
    pub sha1: String,
    /// File size in bytes
    pub size: u64,
    /// Compression type used
    pub compression: FileCompression,
    /// ZFS dataset GUID (if applicable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_guid: Option<String>,
    /// Storage backend (admin-only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stor: Option<StorageType>,
    /// Docker content digest
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Docker uncompressed content digest (deprecated, camelCase exception)
    #[serde(
        default,
        rename = "uncompressedDigest",
        skip_serializing_if = "Option::is_none"
    )]
    pub uncompressed_digest: Option<String>,
}

/// Query parameters for adding an image file
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddImageFileQuery {
    /// Compression of the uploaded file
    #[serde(default)]
    pub compression: Option<FileCompression>,
    /// SHA-1 hash for verification
    #[serde(default)]
    pub sha1: Option<String>,
    /// File size for verification
    #[serde(default)]
    pub size: Option<u64>,
    /// Dataset GUID
    #[serde(default)]
    pub dataset_guid: Option<String>,
    /// Storage backend to use
    #[serde(default)]
    pub storage: Option<StorageType>,
    /// Source IMGAPI URL to fetch the file from (mutually exclusive with body upload)
    #[serde(default)]
    pub source: Option<String>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}

/// Request body for adding an image file from a URL
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AddImageFileFromUrlRequest {
    /// URL to fetch the image file from
    pub file_url: String,
    /// Compression of the file at the URL
    #[serde(default)]
    pub compression: Option<FileCompression>,
    /// SHA-1 hash for verification
    #[serde(default)]
    pub sha1: Option<String>,
    /// File size for verification
    #[serde(default)]
    pub size: Option<u64>,
    /// Dataset GUID
    #[serde(default)]
    pub dataset_guid: Option<String>,
    /// Storage backend to use
    #[serde(default)]
    pub storage: Option<StorageType>,
}

/// Query parameters for adding an image icon
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddImageIconQuery {
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}
