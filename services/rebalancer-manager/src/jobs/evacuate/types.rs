// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Types for evacuate job tracking

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::str::FromStr;

use rebalancer_types::ObjectSkippedReason;

/// Status of an object in the evacuate process
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvacuateObjectStatus {
    /// Default state - object discovered but not yet processed
    #[default]
    Unprocessed,
    /// Object has been included in an assignment
    Assigned,
    /// Could not find a shark to put this object in, will retry
    Skipped,
    /// A persistent error has occurred
    Error,
    /// Updating metadata and any other postprocessing steps
    PostProcessing,
    /// Object has been fully rebalanced
    Complete,
}

impl fmt::Display for EvacuateObjectStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unprocessed => write!(f, "unprocessed"),
            Self::Assigned => write!(f, "assigned"),
            Self::Skipped => write!(f, "skipped"),
            Self::Error => write!(f, "error"),
            Self::PostProcessing => write!(f, "post_processing"),
            Self::Complete => write!(f, "complete"),
        }
    }
}

impl FromStr for EvacuateObjectStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unprocessed" => Ok(Self::Unprocessed),
            "assigned" => Ok(Self::Assigned),
            "skipped" => Ok(Self::Skipped),
            "error" => Ok(Self::Error),
            "post_processing" => Ok(Self::PostProcessing),
            "complete" => Ok(Self::Complete),
            _ => Err(format!("Unknown status: {}", s)),
        }
    }
}

/// Errors that can occur during object evacuation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvacuateObjectError {
    /// Could not get Moray client for shard
    BadMorayClient,
    /// Moray object is malformed
    BadMorayObject,
    /// Manta object is malformed
    BadMantaObject,
    /// Shard number is invalid
    BadShardNumber,
    /// Object would be duplicated on a shark
    DuplicateShark,
    /// Internal error occurred
    InternalError,
    /// Metadata update failed
    MetadataUpdateFailed,
    /// Object has no sharks in metadata
    MissingSharks,
    /// Content length is invalid
    BadContentLength,
}

impl fmt::Display for EvacuateObjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMorayClient => write!(f, "bad_moray_client"),
            Self::BadMorayObject => write!(f, "bad_moray_object"),
            Self::BadMantaObject => write!(f, "bad_manta_object"),
            Self::BadShardNumber => write!(f, "bad_shard_number"),
            Self::DuplicateShark => write!(f, "duplicate_shark"),
            Self::InternalError => write!(f, "internal_error"),
            Self::MetadataUpdateFailed => write!(f, "metadata_update_failed"),
            Self::MissingSharks => write!(f, "missing_sharks"),
            Self::BadContentLength => write!(f, "bad_content_length"),
        }
    }
}

impl FromStr for EvacuateObjectError {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "bad_moray_client" => Ok(Self::BadMorayClient),
            "bad_moray_object" => Ok(Self::BadMorayObject),
            "bad_manta_object" => Ok(Self::BadMantaObject),
            "bad_shard_number" => Ok(Self::BadShardNumber),
            "duplicate_shark" => Ok(Self::DuplicateShark),
            "internal_error" => Ok(Self::InternalError),
            "metadata_update_failed" => Ok(Self::MetadataUpdateFailed),
            "missing_sharks" => Ok(Self::MissingSharks),
            "bad_content_length" => Ok(Self::BadContentLength),
            _ => Err(format!("Unknown error: {}", s)),
        }
    }
}

/// An object being evacuated from a storage node
///
/// This wraps a Manta object with additional tracking state for the
/// evacuation process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvacuateObject {
    /// MantaObject objectId (primary key)
    pub id: String,

    /// UUID of the assignment this object is part of
    pub assignment_id: String,

    /// The Manta object being rebalanced (JSON value)
    pub object: Value,

    /// Shard number of the metadata object record
    pub shard: i32,

    /// Destination shark storage ID
    pub dest_shark: String,

    /// Moray object etag
    pub etag: String,

    /// Current status in the evacuation process
    pub status: EvacuateObjectStatus,

    /// Reason if the object was skipped
    pub skipped_reason: Option<ObjectSkippedReason>,

    /// Error if one occurred
    pub error: Option<EvacuateObjectError>,
}

impl Default for EvacuateObject {
    fn default() -> Self {
        Self {
            id: String::new(),
            assignment_id: String::new(),
            object: Value::Null,
            shard: 0,
            dest_shark: String::new(),
            etag: String::new(),
            status: EvacuateObjectStatus::default(),
            skipped_reason: None,
            error: None,
        }
    }
}

/// Essential fields extracted from a Manta object
///
/// These are the minimum fields needed to rebalance an object.
#[derive(Debug, Clone, Deserialize)]
pub struct MantaObjectEssential {
    pub key: String,
    pub owner: String,

    #[serde(alias = "contentLength", default)]
    pub content_length: u64,

    #[serde(alias = "contentMD5", default)]
    pub content_md5: String,

    #[serde(alias = "objectId", default)]
    pub object_id: String,

    #[serde(default)]
    pub etag: String,

    #[serde(default)]
    pub sharks: Vec<MantaObjectShark>,
}

/// Shark entry from a Manta object's sharks array
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MantaObjectShark {
    pub manta_storage_id: String,
    pub datacenter: String,
}

impl Default for MantaObjectShark {
    fn default() -> Self {
        Self {
            manta_storage_id: String::new(),
            datacenter: String::new(),
        }
    }
}
