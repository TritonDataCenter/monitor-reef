/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2026 Edgecast Cloud LLC.
 */

//! MPU (Multipart Upload) Utilities
//!
//! This module provides utilities for handling multipart upload objects during
//! shark evacuation. MPU objects use special key patterns that need to be
//! recognized and parsed to update upload records when parts are evacuated.
//!
//! # MPU Object Patterns
//!
//! - **Part objects**: `.mpu-parts/{uploadId}/{partNumber}`
//!   - Example: `.mpu-parts/abc-123-def/1`
//!   - These are the actual part data objects that get evacuated
//!
//! - **Upload records**: `.mpu-uploads/{uploadId}`
//!   - Example: `.mpu-uploads/abc-123-def`
//!   - These contain cached shark locations that must be updated
//!
//! # Usage
//!
//! ```rust,ignore
//! use mpu_utils::{parse_mpu_part_key, is_mpu_part};
//!
//! if let Some(upload_id) = parse_mpu_part_key(&object.key) {
//!     // Track this uploadId for upload record update
//!     tracker.record_upload_id(upload_id);
//! }
//! ```

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

use crate::mdapi_client;
use libmanta::mdapi::{MdapiClient, MdapiError};
use rebalancer::error::{Error, InternalError, InternalErrorCode};

/// Regex pattern for MPU part keys: `.mpu-parts/{uploadId}/{partNumber}`
///
/// Captures the uploadId from part object keys.
/// Pattern breakdown:
/// - `^\.mpu-parts/` - Must start with `.mpu-parts/`
/// - `([^/]+)` - Capture group 1: uploadId (any chars except `/`)
/// - `/` - Separator
/// - `\d+` - Part number (digits only)
/// - `$` - End of string
const MPU_PART_KEY_PATTERN: &str = r"^\.mpu-parts/([^/]+)/\d+$";

/// Regex pattern for MPU upload record keys: `.mpu-uploads/{uploadId}`
///
/// Captures the uploadId from upload record object keys.
/// Pattern breakdown:
/// - `^\.mpu-uploads/` - Must start with `.mpu-uploads/`
/// - `([^/]+)` - Capture group 1: uploadId (any chars except `/`)
/// - `$` - End of string (no trailing parts)
const MPU_UPLOAD_KEY_PATTERN: &str = r"^\.mpu-uploads/([^/]+)$";

/// Parse an MPU part key to extract the uploadId.
///
/// # Arguments
///
/// * `key` - The object key to parse
///
/// # Returns
///
/// * `Some(String)` - The extracted uploadId if the key matches the MPU part
///   pattern
/// * `None` - If the key does not match the MPU part pattern
///
/// # Examples
///
/// ```rust,ignore
/// assert_eq!(
///     parse_mpu_part_key(".mpu-parts/abc-123/1"),
///     Some("abc-123".to_string())
/// );
/// assert_eq!(parse_mpu_part_key("regular-object.txt"), None);
/// assert_eq!(parse_mpu_part_key(".mpu-uploads/abc-123"), None);
/// ```
///
/// # Performance
///
/// This function compiles the regex on first call and caches it. Subsequent
/// calls reuse the compiled regex. Cost: O(n) where n is key length.
pub fn parse_mpu_part_key(key: &str) -> Option<String> {
    lazy_static::lazy_static! {
        static ref MPU_PART_RE: Regex =
            Regex::new(MPU_PART_KEY_PATTERN).expect("Invalid MPU part regex");
    }

    MPU_PART_RE
        .captures(key)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Parse an MPU upload record key to extract the uploadId.
///
/// # Arguments
///
/// * `key` - The object key to parse
///
/// # Returns
///
/// * `Some(String)` - The extracted uploadId if the key matches the MPU upload
///   record pattern
/// * `None` - If the key does not match the MPU upload record pattern
///
/// # Examples
///
/// ```rust,ignore
/// assert_eq!(
///     parse_mpu_upload_key(".mpu-uploads/abc-123"),
///     Some("abc-123".to_string())
/// );
/// assert_eq!(parse_mpu_upload_key("regular-object.txt"), None);
/// assert_eq!(parse_mpu_upload_key(".mpu-parts/abc-123/1"), None);
/// ```
///
/// # Performance
///
/// This function compiles the regex on first call and caches it. Subsequent
/// calls reuse the compiled regex. Cost: O(n) where n is key length.
pub fn parse_mpu_upload_key(key: &str) -> Option<String> {
    lazy_static::lazy_static! {
        static ref MPU_UPLOAD_RE: Regex =
            Regex::new(MPU_UPLOAD_KEY_PATTERN).expect("Invalid MPU upload regex");
    }

    MPU_UPLOAD_RE
        .captures(key)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Check if an object key represents an MPU part.
///
/// This is a fast predicate function that returns true if the key matches the
/// MPU part pattern. Use this when you only need to check if an object is an
/// MPU part without extracting the uploadId.
///
/// # Arguments
///
/// * `key` - The object key to check
///
/// # Returns
///
/// * `true` - If the key matches the MPU part pattern
/// * `false` - If the key does not match
///
/// # Examples
///
/// ```rust,ignore
/// assert!(is_mpu_part(".mpu-parts/abc-123/5"));
/// assert!(!is_mpu_part(".mpu-uploads/abc-123"));
/// assert!(!is_mpu_part("regular-object.txt"));
/// ```
///
/// # Performance
///
/// Cost: O(n) where n is key length. Slightly more efficient than calling
/// `parse_mpu_part_key()` and checking for Some when the uploadId is not
/// needed.
pub fn is_mpu_part(key: &str) -> bool {
    lazy_static::lazy_static! {
        static ref MPU_PART_RE: Regex =
            Regex::new(MPU_PART_KEY_PATTERN).expect("Invalid MPU part regex");
    }

    MPU_PART_RE.is_match(key)
}

/// Check if an object key represents an MPU upload record.
///
/// This is a fast predicate function that returns true if the key matches the
/// MPU upload record pattern.
///
/// # Arguments
///
/// * `key` - The object key to check
///
/// # Returns
///
/// * `true` - If the key matches the MPU upload record pattern
/// * `false` - If the key does not match
///
/// # Examples
///
/// ```rust,ignore
/// assert!(is_mpu_upload_record(".mpu-uploads/abc-123"));
/// assert!(!is_mpu_upload_record(".mpu-parts/abc-123/1"));
/// assert!(!is_mpu_upload_record("regular-object.txt"));
/// ```
///
/// # Performance
///
/// Cost: O(n) where n is key length.
pub fn is_mpu_upload_record(key: &str) -> bool {
    lazy_static::lazy_static! {
        static ref MPU_UPLOAD_RE: Regex =
            Regex::new(MPU_UPLOAD_KEY_PATTERN).expect("Invalid MPU upload regex");
    }

    MPU_UPLOAD_RE.is_match(key)
}

/// MPU Evacuation Tracker
///
/// Tracks evacuated MPU parts during batch processing to efficiently update
/// upload records. When multiple parts from the same uploadId are evacuated,
/// this tracker deduplicates so that each upload record is updated once per
/// batch.
///
/// The tracker records **which upload IDs were affected** rather than shark
/// lists, because the correct update is a selective shark replacement in the
/// upload record's `preAllocatedSharks` array (from_shark → dest_shark),
/// not a bulk overwrite.
///
/// # Usage
///
/// ```rust,ignore
/// let mut tracker = MpuEvacuationTracker::new();
///
/// // During evacuation loop
/// if let Some(upload_id) = parse_mpu_part_key(&object.key) {
///     tracker.record_upload_id(upload_id);
/// }
///
/// // After batch completes
/// for upload_id in tracker.get_affected_uploads() {
///     replace_shark_in_upload_record(upload_id, from_shark, dest_shark)?;
/// }
/// ```
///
/// # Performance
///
/// - Uses HashMap for O(1) uploadId deduplication
/// - Minimal memory overhead (one entry per unique uploadId)
#[derive(Debug, Default)]
pub struct MpuEvacuationTracker {
    /// Set of uploadIds with evacuated parts (deduplication via HashMap)
    upload_ids: HashMap<String, ()>,
}

impl MpuEvacuationTracker {
    /// Create a new empty evacuation tracker.
    pub fn new() -> Self {
        MpuEvacuationTracker {
            upload_ids: HashMap::new(),
        }
    }

    /// Record that an MPU part with the given uploadId was evacuated.
    ///
    /// Duplicate uploadIds are deduplicated automatically.
    pub fn record_upload_id(&mut self, upload_id: String) {
        self.upload_ids.insert(upload_id, ());
    }

    /// Get all affected uploadIds.
    ///
    /// Returns an iterator over uploadId strings for all recorded
    /// evacuations. The caller is responsible for applying the correct
    /// from→dest shark replacement to each upload record.
    pub fn get_affected_uploads(&self) -> impl Iterator<Item = &String> {
        self.upload_ids.keys()
    }

    /// Get the count of unique uploadIds tracked.
    pub fn count(&self) -> usize {
        self.upload_ids.len()
    }

    /// Clear all recorded evacuations.
    pub fn clear(&mut self) {
        self.upload_ids.clear();
    }
}

/// Update an MPU upload record by replacing the evacuated shark in
/// `preAllocatedSharks`.
///
/// During shark evacuation, parts are moved from `from_shark` to
/// `dest_shark`.  The upload record's `preAllocatedSharks` array caches
/// which sharks hold parts for this MPU.  We must selectively replace
/// `from_shark` entries with `dest_shark` — a bulk overwrite would lose
/// sharks unrelated to the evacuation.
///
/// # Arguments
///
/// * `mclient`    - The mdapi client instance
/// * `owner`      - The owner UUID
/// * `bucket_id`  - The bucket UUID containing the upload record
/// * `upload_id`  - The uploadId extracted from MPU part keys
/// * `from_shark` - The manta_storage_id being evacuated **from**
/// * `dest_shark` - The manta_storage_id being evacuated **to**
///
/// # Error Handling
///
/// - **Upload record not found**: Returns Ok (record may have been
///   cleaned up after MPU completion/abort).
/// - **JSON parse failure / Update RPC failure**: Returns Err.
///
/// # Performance
///
/// Cost: 2 RPC calls (get + update)
pub fn update_upload_record(
    mclient: &MdapiClient,
    owner: Uuid,
    bucket_id: Uuid,
    upload_id: &str,
    from_shark: &str,
    dest_shark: &str,
) -> Result<(), Error> {
    // Validate upload_id to prevent path traversal.
    if upload_id.is_empty()
        || upload_id.contains('/')
        || upload_id.contains('\0')
        || upload_id.contains("..")
    {
        return Err(Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!("Invalid upload_id: {:?}", upload_id),
        )));
    }

    let upload_key = format!(".mpu-uploads/{}", upload_id);

    debug!(
        "Updating upload record: {}, replacing shark {} -> {}",
        upload_key, from_shark, dest_shark
    );

    // Fetch the upload record
    let (_object, content) = match mdapi_client::get_object_with_content(
        mclient,
        owner,
        bucket_id,
        &upload_key,
    ) {
        Ok(result) => result,
        Err(e) => {
            let is_not_found =
                matches!(&e, Error::Mdapi(MdapiError::ObjectNotFound(_)));
            if is_not_found {
                warn!(
                    "Upload record not found: {} (likely already cleaned up)",
                    upload_key
                );
                return Ok(());
            } else {
                error!(
                    "Failed to fetch upload record {}: {:?}",
                    upload_key, e
                );
                return Err(e);
            }
        }
    };

    // Parse JSON content
    let mut upload_record: Value = match serde_json::from_str(&content) {
        Ok(json) => json,
        Err(e) => {
            error!(
                "Failed to parse upload record JSON for {}: {}",
                upload_key, e
            );
            return Err(Error::from(e));
        }
    };

    // Selectively replace from_shark with dest_shark in preAllocatedSharks
    let mut replaced = false;
    if let Some(sharks) = upload_record
        .get_mut("preAllocatedSharks")
        .and_then(|v| v.as_array_mut())
    {
        for shark in sharks.iter_mut() {
            let matches = shark
                .get("manta_storage_id")
                .and_then(|v| v.as_str())
                .map(|id| id == from_shark)
                .unwrap_or(false);
            if matches {
                shark["manta_storage_id"] =
                    Value::String(dest_shark.to_string());
                replaced = true;
            }
        }
    }

    if !replaced {
        debug!(
            "Upload record {} had no preAllocatedSharks entry for {}; \
             skipping update",
            upload_key, from_shark
        );
        return Ok(());
    }

    // Serialize back to string
    let updated_content = match serde_json::to_string(&upload_record) {
        Ok(s) => s,
        Err(e) => {
            error!(
                "Failed to serialize updated upload record for {}: {}",
                upload_key, e
            );
            return Err(Error::from(e));
        }
    };

    // Write back to mdapi
    mdapi_client::update_object_content(
        mclient,
        owner,
        bucket_id,
        &upload_key,
        &updated_content,
    )?;

    info!(
        "Successfully updated upload record: {} (replaced {} -> {})",
        upload_key, from_shark, dest_shark
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test parse_mpu_part_key with valid MPU part keys
    #[test]
    fn test_parse_mpu_part_key_valid() {
        assert_eq!(
            parse_mpu_part_key(".mpu-parts/abc-123/1"),
            Some("abc-123".to_string())
        );
        assert_eq!(
            parse_mpu_part_key(".mpu-parts/upload-id-with-dashes/999"),
            Some("upload-id-with-dashes".to_string())
        );
        assert_eq!(
            parse_mpu_part_key(".mpu-parts/simple/0"),
            Some("simple".to_string())
        );
    }

    // Test parse_mpu_part_key with invalid keys
    #[test]
    fn test_parse_mpu_part_key_invalid() {
        assert_eq!(parse_mpu_part_key("regular-object.txt"), None);
        assert_eq!(parse_mpu_part_key(".mpu-uploads/abc-123"), None);
        assert_eq!(parse_mpu_part_key(".mpu-parts/"), None);
        assert_eq!(parse_mpu_part_key(".mpu-parts/id"), None);
        assert_eq!(parse_mpu_part_key(".mpu-parts/id/"), None);
        assert_eq!(parse_mpu_part_key(".mpu-parts/id/abc"), None);
        assert_eq!(parse_mpu_part_key("mpu-parts/id/1"), None);
    }

    // Test parse_mpu_upload_key with valid upload record keys
    #[test]
    fn test_parse_mpu_upload_key_valid() {
        assert_eq!(
            parse_mpu_upload_key(".mpu-uploads/abc-123"),
            Some("abc-123".to_string())
        );
        assert_eq!(
            parse_mpu_upload_key(".mpu-uploads/upload-id-with-dashes"),
            Some("upload-id-with-dashes".to_string())
        );
        assert_eq!(
            parse_mpu_upload_key(".mpu-uploads/simple"),
            Some("simple".to_string())
        );
    }

    // Test parse_mpu_upload_key with invalid keys
    #[test]
    fn test_parse_mpu_upload_key_invalid() {
        assert_eq!(parse_mpu_upload_key("regular-object.txt"), None);
        assert_eq!(parse_mpu_upload_key(".mpu-parts/abc-123/1"), None);
        assert_eq!(parse_mpu_upload_key(".mpu-uploads/"), None);
        assert_eq!(parse_mpu_upload_key(".mpu-uploads/id/extra"), None);
        assert_eq!(parse_mpu_upload_key("mpu-uploads/id"), None);
    }

    // Test is_mpu_part predicate
    #[test]
    fn test_is_mpu_part() {
        assert!(is_mpu_part(".mpu-parts/abc-123/5"));
        assert!(is_mpu_part(".mpu-parts/id/0"));
        assert!(is_mpu_part(".mpu-parts/id/999999"));

        assert!(!is_mpu_part(".mpu-uploads/abc-123"));
        assert!(!is_mpu_part("regular-object.txt"));
        assert!(!is_mpu_part(".mpu-parts/"));
        assert!(!is_mpu_part(".mpu-parts/id"));
        assert!(!is_mpu_part(".mpu-parts/id/abc"));
    }

    // Test is_mpu_upload_record predicate
    #[test]
    fn test_is_mpu_upload_record() {
        assert!(is_mpu_upload_record(".mpu-uploads/abc-123"));
        assert!(is_mpu_upload_record(".mpu-uploads/id"));
        assert!(is_mpu_upload_record(".mpu-uploads/upload-with-dashes"));

        assert!(!is_mpu_upload_record(".mpu-parts/abc-123/1"));
        assert!(!is_mpu_upload_record("regular-object.txt"));
        assert!(!is_mpu_upload_record(".mpu-uploads/"));
        assert!(!is_mpu_upload_record(".mpu-uploads/id/extra"));
    }

    // Test edge cases with special characters in uploadId
    #[test]
    fn test_special_characters_in_upload_id() {
        // UUIDs with dashes are common
        assert_eq!(
            parse_mpu_part_key(
                ".mpu-parts/550e8400-e29b-41d4-a716-446655440000/1"
            ),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );

        // Underscores and alphanumeric
        assert_eq!(
            parse_mpu_part_key(".mpu-parts/upload_id_123/1"),
            Some("upload_id_123".to_string())
        );

        // Should not match keys with slashes in uploadId (invalid)
        assert_eq!(parse_mpu_part_key(".mpu-parts/id/with/slashes/1"), None);
    }

    // Test consistency between parsing and predicate functions
    #[test]
    fn test_consistency() {
        let test_keys = vec![
            ".mpu-parts/abc-123/1",
            ".mpu-uploads/abc-123",
            "regular-object.txt",
            ".mpu-parts/id",
            ".mpu-uploads/id/extra",
        ];

        for key in test_keys {
            // If parse returns Some, predicate should return true
            let parsed_part = parse_mpu_part_key(key);
            let is_part = is_mpu_part(key);
            assert_eq!(
                parsed_part.is_some(),
                is_part,
                "Inconsistency for key: {}",
                key
            );

            let parsed_upload = parse_mpu_upload_key(key);
            let is_upload = is_mpu_upload_record(key);
            assert_eq!(
                parsed_upload.is_some(),
                is_upload,
                "Inconsistency for key: {}",
                key
            );
        }
    }

    // Test MpuEvacuationTracker basic operations
    #[test]
    fn test_tracker_new() {
        let tracker = MpuEvacuationTracker::new();
        assert_eq!(tracker.count(), 0);
    }

    #[test]
    fn test_tracker_record_single() {
        let mut tracker = MpuEvacuationTracker::new();

        tracker.record_upload_id("upload-123".to_string());
        assert_eq!(tracker.count(), 1);

        let affected: Vec<_> = tracker.get_affected_uploads().collect();
        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0], "upload-123");
    }

    #[test]
    fn test_tracker_record_multiple_different_uploads() {
        let mut tracker = MpuEvacuationTracker::new();

        tracker.record_upload_id("upload-1".to_string());
        tracker.record_upload_id("upload-2".to_string());

        assert_eq!(tracker.count(), 2);

        let affected: std::collections::HashSet<_> =
            tracker.get_affected_uploads().collect();
        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&"upload-1".to_string()));
        assert!(affected.contains(&"upload-2".to_string()));
    }

    #[test]
    fn test_tracker_record_same_upload_twice_deduplicates() {
        let mut tracker = MpuEvacuationTracker::new();

        tracker.record_upload_id("upload-123".to_string());
        tracker.record_upload_id("upload-123".to_string());

        // Should still have only 1 uploadId (deduplicated)
        assert_eq!(tracker.count(), 1);

        let affected: Vec<_> = tracker.get_affected_uploads().collect();
        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0], "upload-123");
    }

    #[test]
    fn test_tracker_clear() {
        let mut tracker = MpuEvacuationTracker::new();

        tracker.record_upload_id("upload-1".to_string());
        tracker.record_upload_id("upload-2".to_string());
        assert_eq!(tracker.count(), 2);

        tracker.clear();
        assert_eq!(tracker.count(), 0);

        let affected: Vec<_> = tracker.get_affected_uploads().collect();
        assert_eq!(affected.len(), 0);
    }

    #[test]
    fn test_tracker_get_affected_uploads_iteration() {
        let mut tracker = MpuEvacuationTracker::new();

        tracker.record_upload_id("upload-1".to_string());
        tracker.record_upload_id("upload-2".to_string());
        tracker.record_upload_id("upload-3".to_string());

        let mut count = 0;
        let mut seen_ids = std::collections::HashSet::new();

        for upload_id in tracker.get_affected_uploads() {
            count += 1;
            seen_ids.insert(upload_id.clone());
        }

        assert_eq!(count, 3);
        assert_eq!(seen_ids.len(), 3);
        assert!(seen_ids.contains("upload-1"));
        assert!(seen_ids.contains("upload-2"));
        assert!(seen_ids.contains("upload-3"));
    }

    // =========================================================================
    // Tests for selective shark replacement JSON manipulation logic
    // These tests verify the from→dest shark replacement that
    // update_upload_record performs, without requiring RPC calls.
    // =========================================================================

    /// Helper: apply selective shark replacement (same logic as
    /// update_upload_record) on an in-memory JSON Value.
    fn apply_shark_replacement(
        upload_record: &mut serde_json::Value,
        from_shark: &str,
        dest_shark: &str,
    ) -> bool {
        let mut replaced = false;
        if let Some(sharks) = upload_record
            .get_mut("preAllocatedSharks")
            .and_then(|v| v.as_array_mut())
        {
            for shark in sharks.iter_mut() {
                let matches = shark
                    .get("manta_storage_id")
                    .and_then(|v| v.as_str())
                    .map(|id| id == from_shark)
                    .unwrap_or(false);
                if matches {
                    shark["manta_storage_id"] =
                        Value::String(dest_shark.to_string());
                    replaced = true;
                }
            }
        }
        replaced
    }

    #[test]
    fn test_selective_replacement_single_shark() {
        // One shark in the array matches from_shark
        let original = r#"{
            "uploadId": "abc-123-def",
            "state": "created",
            "preAllocatedSharks": [
                {"datacenter": "dc1", "manta_storage_id": "old.stor.domain"}
            ]
        }"#;

        let mut record: serde_json::Value =
            serde_json::from_str(original).unwrap();

        let replaced =
            apply_shark_replacement(&mut record, "old.stor.domain", "new.stor.domain");
        assert!(replaced);

        let sharks = record["preAllocatedSharks"].as_array().unwrap();
        assert_eq!(sharks.len(), 1);
        assert_eq!(
            sharks[0]["manta_storage_id"].as_str().unwrap(),
            "new.stor.domain"
        );
        // Other fields preserved
        assert_eq!(sharks[0]["datacenter"].as_str().unwrap(), "dc1");
        assert_eq!(record["uploadId"].as_str().unwrap(), "abc-123-def");
        assert_eq!(record["state"].as_str().unwrap(), "created");
    }

    #[test]
    fn test_selective_replacement_preserves_unrelated_sharks() {
        // Two sharks: only the matching one is replaced
        let original = r#"{
            "uploadId": "multi-shark",
            "preAllocatedSharks": [
                {"datacenter": "dc1", "manta_storage_id": "old.stor.domain"},
                {"datacenter": "dc2", "manta_storage_id": "other.stor.domain"}
            ]
        }"#;

        let mut record: serde_json::Value =
            serde_json::from_str(original).unwrap();

        let replaced =
            apply_shark_replacement(&mut record, "old.stor.domain", "new.stor.domain");
        assert!(replaced);

        let sharks = record["preAllocatedSharks"].as_array().unwrap();
        assert_eq!(sharks.len(), 2);
        assert_eq!(
            sharks[0]["manta_storage_id"].as_str().unwrap(),
            "new.stor.domain"
        );
        // Second shark is untouched
        assert_eq!(
            sharks[1]["manta_storage_id"].as_str().unwrap(),
            "other.stor.domain"
        );
    }

    #[test]
    fn test_selective_replacement_no_match() {
        // from_shark not present → no replacement, returns false
        let original = r#"{
            "uploadId": "no-match",
            "preAllocatedSharks": [
                {"datacenter": "dc1", "manta_storage_id": "unrelated.stor.domain"}
            ]
        }"#;

        let mut record: serde_json::Value =
            serde_json::from_str(original).unwrap();

        let replaced =
            apply_shark_replacement(&mut record, "old.stor.domain", "new.stor.domain");
        assert!(!replaced);

        // Array unchanged
        let sharks = record["preAllocatedSharks"].as_array().unwrap();
        assert_eq!(
            sharks[0]["manta_storage_id"].as_str().unwrap(),
            "unrelated.stor.domain"
        );
    }

    #[test]
    fn test_selective_replacement_missing_pre_allocated_sharks() {
        // No preAllocatedSharks field at all → no replacement
        let original = r#"{
            "uploadId": "xyz-789",
            "state": "created"
        }"#;

        let mut record: serde_json::Value =
            serde_json::from_str(original).unwrap();

        let replaced =
            apply_shark_replacement(&mut record, "old.stor.domain", "new.stor.domain");
        assert!(!replaced);
    }

    #[test]
    fn test_selective_replacement_roundtrip_serialization() {
        let original = r#"{"uploadId":"test-123","preAllocatedSharks":[{"manta_storage_id":"old.stor"}]}"#;

        let mut record: serde_json::Value =
            serde_json::from_str(original).unwrap();

        apply_shark_replacement(&mut record, "old.stor", "new.stor");

        // Serialize back and re-parse
        let serialized = serde_json::to_string(&record).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).unwrap();

        assert_eq!(parsed["uploadId"].as_str().unwrap(), "test-123");
        assert_eq!(
            parsed["preAllocatedSharks"][0]["manta_storage_id"]
                .as_str()
                .unwrap(),
            "new.stor"
        );
    }

    #[test]
    fn test_upload_key_format() {
        let upload_id = "abc-123-def-456";
        let upload_key = format!(".mpu-uploads/{}", upload_id);
        assert_eq!(upload_key, ".mpu-uploads/abc-123-def-456");
    }
}
