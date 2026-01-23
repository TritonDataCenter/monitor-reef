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
//!     tracker.record_evacuation(upload_id, new_sharks);
//! }
//! ```

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

use crate::mdapi_client;
use crate::storinfo::StorageNode;
use libmanta::mdapi::MdapiClient;
use rebalancer::error::Error;

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
/// this tracker deduplicates and merges shark information.
///
/// # Usage
///
/// ```rust,ignore
/// let mut tracker = MpuEvacuationTracker::new();
///
/// // During evacuation loop
/// if let Some(upload_id) = parse_mpu_part_key(&object.key) {
///     tracker.record_evacuation(upload_id, new_sharks.clone());
/// }
///
/// // After batch completes
/// for (upload_id, sharks) in tracker.get_affected_uploads() {
///     update_upload_record(upload_id, sharks)?;
/// }
/// ```
///
/// # Performance
///
/// - Uses HashMap for O(1) uploadId lookups
/// - Deduplicates automatically (same uploadId recorded multiple times)
/// - Minimal memory overhead (one entry per unique uploadId)
#[derive(Debug, Default)]
pub struct MpuEvacuationTracker {
    /// Maps uploadId to the new shark locations for evacuated parts
    upload_records: HashMap<String, Vec<StorageNode>>,
}

impl MpuEvacuationTracker {
    /// Create a new empty evacuation tracker.
    ///
    /// # Returns
    ///
    /// A new `MpuEvacuationTracker` with no recorded evacuations.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let tracker = MpuEvacuationTracker::new();
    /// assert_eq!(tracker.count(), 0);
    /// ```
    pub fn new() -> Self {
        MpuEvacuationTracker {
            upload_records: HashMap::new(),
        }
    }

    /// Record an evacuation for an MPU part.
    ///
    /// If this uploadId has already been recorded, the shark list is updated
    /// with the new sharks. For the same uploadId, the most recent shark list
    /// is used (last-write-wins).
    ///
    /// # Arguments
    ///
    /// * `upload_id` - The uploadId extracted from the MPU part key
    /// * `sharks` - The new shark locations where the part was evacuated to
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let mut tracker = MpuEvacuationTracker::new();
    /// tracker.record_evacuation("upload-123".to_string(), sharks);
    /// assert_eq!(tracker.count(), 1);
    /// ```
    ///
    /// # Invariants
    ///
    /// - Each uploadId maps to exactly one shark list (the most recent)
    /// - Recording the same uploadId multiple times overwrites the previous
    ///   sharks
    pub fn record_evacuation(
        &mut self,
        upload_id: String,
        sharks: Vec<StorageNode>,
    ) {
        self.upload_records.insert(upload_id, sharks);
    }

    /// Get all affected uploadIds and their new shark locations.
    ///
    /// Returns an iterator over (uploadId, sharks) pairs for all recorded
    /// evacuations. This is typically called after a batch of evacuations
    /// completes to update all affected upload records.
    ///
    /// # Returns
    ///
    /// An iterator over references to (String, Vec<StorageNode>) pairs.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// for (upload_id, sharks) in tracker.get_affected_uploads() {
    ///     update_upload_record(upload_id, sharks)?;
    /// }
    /// ```
    ///
    /// # Performance
    ///
    /// Cost: O(1) to create the iterator, O(n) to iterate over n uploadIds.
    pub fn get_affected_uploads(
        &self,
    ) -> impl Iterator<Item = (&String, &Vec<StorageNode>)> {
        self.upload_records.iter()
    }

    /// Get the count of unique uploadIds tracked.
    ///
    /// # Returns
    ///
    /// The number of unique uploadIds that have been recorded.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let mut tracker = MpuEvacuationTracker::new();
    /// assert_eq!(tracker.count(), 0);
    /// tracker.record_evacuation("upload-1".to_string(), sharks);
    /// assert_eq!(tracker.count(), 1);
    /// ```
    pub fn count(&self) -> usize {
        self.upload_records.len()
    }

    /// Clear all recorded evacuations.
    ///
    /// This is typically called after successfully updating all upload records
    /// to prepare for the next evacuation batch.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// tracker.record_evacuation("upload-1".to_string(), sharks);
    /// assert_eq!(tracker.count(), 1);
    /// tracker.clear();
    /// assert_eq!(tracker.count(), 0);
    /// ```
    pub fn clear(&mut self) {
        self.upload_records.clear();
    }
}

/// Update an MPU upload record's preAllocatedSharks with new shark locations.
///
/// This function is called after evacuating MPU parts to update the upload
/// record's cached shark information. It fetches the upload record, parses its
/// JSON content, updates the preAllocatedSharks array, and writes it back.
///
/// # Arguments
///
/// * `mclient` - The mdapi client instance
/// * `owner` - The owner UUID
/// * `bucket_id` - The bucket UUID containing the upload record
/// * `upload_id` - The uploadId extracted from MPU part keys
/// * `new_sharks` - The new shark locations to set
///
/// # Returns
///
/// * `Ok(())` - Upload record successfully updated
/// * `Err(Error)` - Update failed (see error handling below)
///
/// # Error Handling
///
/// This function implements graceful error handling to avoid failing the
/// entire evacuation if an upload record update fails:
///
/// - **Upload record not found (404)**: Log warning and return Ok.
///   Reason: Upload record may have been cleaned up if MPU completed/aborted.
///   Impact: MPU completion may fail, but object data is preserved.
///   Recovery: User can re-upload parts via S3 client retry.
///
/// - **JSON parse failure**: Log error and return Err.
///   Reason: Upload record corrupted or schema mismatch.
///   Impact: MPU completion will likely fail.
///   Recovery: Manual investigation needed.
///
/// - **Update RPC failure**: Log error and return Err.
///   Reason: Network or mdapi service issue.
///   Impact: MPU completion will fail.
///   Recovery: Retry evacuation or manual upload record update.
///
/// # Examples
///
/// ```rust,ignore
/// // After evacuating parts for uploadId "abc-123"
/// update_upload_record(
///     &client,
///     owner,
///     bucket_id,
///     "abc-123",
///     &new_sharks
/// )?;
/// ```
///
/// # Invariants
///
/// - Upload record JSON structure is preserved except `preAllocatedSharks`
/// - All sharks in `new_sharks` are written to the upload record
/// - Update is atomic (single mdapi RPC)
///
/// # Performance
///
/// Cost: O(1) - 2 RPC calls (get + update)
/// Typical latency: ~10-50ms depending on network
pub fn update_upload_record(
    mclient: &MdapiClient,
    owner: Uuid,
    bucket_id: Uuid,
    upload_id: &str,
    new_sharks: &[StorageNode],
) -> Result<(), Error> {
    let upload_key = format!(".mpu-uploads/{}", upload_id);

    debug!(
        "Updating upload record: {}, new_sharks count: {}",
        upload_key,
        new_sharks.len()
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
            // Check if error is 404 (upload record not found)
            let error_msg = format!("{:?}", e);
            if error_msg.contains("404") || error_msg.contains("NotFound") {
                warn!(
                    "Upload record not found: {} (likely already cleaned up)",
                    upload_key
                );
                // Return Ok - upload record cleanup is expected behavior
                return Ok(());
            } else {
                error!("Failed to fetch upload record {}: {:?}", upload_key, e);
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

    // Serialize new sharks to JSON
    let sharks_value = match serde_json::to_value(new_sharks) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to serialize new sharks: {}", e);
            return Err(Error::from(e));
        }
    };

    // Update preAllocatedSharks field
    upload_record["preAllocatedSharks"] = sharks_value;

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
        "Successfully updated upload record: {} with {} sharks",
        upload_key,
        new_sharks.len()
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
        let sharks = vec![StorageNode {
            available_mb: 1000,
            percent_used: 50,
            filesystem: "zfs".to_string(),
            datacenter: "dc1".to_string(),
            manta_storage_id: "1.stor.example.com".to_string(),
            timestamp: 12345,
        }];

        tracker.record_evacuation("upload-123".to_string(), sharks.clone());
        assert_eq!(tracker.count(), 1);

        let affected: Vec<_> = tracker.get_affected_uploads().collect();
        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].0, "upload-123");
        assert_eq!(affected[0].1.len(), 1);
        assert_eq!(affected[0].1[0].manta_storage_id, "1.stor.example.com");
    }

    #[test]
    fn test_tracker_record_multiple_different_uploads() {
        let mut tracker = MpuEvacuationTracker::new();
        let sharks1 = vec![StorageNode {
            available_mb: 1000,
            percent_used: 50,
            filesystem: "zfs".to_string(),
            datacenter: "dc1".to_string(),
            manta_storage_id: "1.stor.example.com".to_string(),
            timestamp: 12345,
        }];
        let sharks2 = vec![StorageNode {
            available_mb: 2000,
            percent_used: 30,
            filesystem: "zfs".to_string(),
            datacenter: "dc2".to_string(),
            manta_storage_id: "2.stor.example.com".to_string(),
            timestamp: 12346,
        }];

        tracker.record_evacuation("upload-1".to_string(), sharks1);
        tracker.record_evacuation("upload-2".to_string(), sharks2);

        assert_eq!(tracker.count(), 2);

        let affected: HashMap<_, _> =
            tracker.get_affected_uploads().collect();
        assert_eq!(affected.len(), 2);
        assert!(affected.contains_key("upload-1"));
        assert!(affected.contains_key("upload-2"));
    }

    #[test]
    fn test_tracker_record_same_upload_twice_last_wins() {
        let mut tracker = MpuEvacuationTracker::new();
        let sharks1 = vec![StorageNode {
            available_mb: 1000,
            percent_used: 50,
            filesystem: "zfs".to_string(),
            datacenter: "dc1".to_string(),
            manta_storage_id: "1.stor.example.com".to_string(),
            timestamp: 12345,
        }];
        let sharks2 = vec![StorageNode {
            available_mb: 2000,
            percent_used: 30,
            filesystem: "zfs".to_string(),
            datacenter: "dc2".to_string(),
            manta_storage_id: "2.stor.example.com".to_string(),
            timestamp: 12346,
        }];

        tracker.record_evacuation("upload-123".to_string(), sharks1);
        tracker.record_evacuation("upload-123".to_string(), sharks2.clone());

        // Should still have only 1 uploadId (deduplicated)
        assert_eq!(tracker.count(), 1);

        // Should have the second (last) shark list
        let affected: Vec<_> = tracker.get_affected_uploads().collect();
        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].1.len(), 1);
        assert_eq!(affected[0].1[0].manta_storage_id, "2.stor.example.com");
    }

    #[test]
    fn test_tracker_clear() {
        let mut tracker = MpuEvacuationTracker::new();
        let sharks = vec![StorageNode {
            available_mb: 1000,
            percent_used: 50,
            filesystem: "zfs".to_string(),
            datacenter: "dc1".to_string(),
            manta_storage_id: "1.stor.example.com".to_string(),
            timestamp: 12345,
        }];

        tracker.record_evacuation("upload-1".to_string(), sharks.clone());
        tracker.record_evacuation("upload-2".to_string(), sharks);
        assert_eq!(tracker.count(), 2);

        tracker.clear();
        assert_eq!(tracker.count(), 0);

        let affected: Vec<_> = tracker.get_affected_uploads().collect();
        assert_eq!(affected.len(), 0);
    }

    #[test]
    fn test_tracker_get_affected_uploads_iteration() {
        let mut tracker = MpuEvacuationTracker::new();
        let sharks = vec![StorageNode {
            available_mb: 1000,
            percent_used: 50,
            filesystem: "zfs".to_string(),
            datacenter: "dc1".to_string(),
            manta_storage_id: "1.stor.example.com".to_string(),
            timestamp: 12345,
        }];

        tracker.record_evacuation("upload-1".to_string(), sharks.clone());
        tracker.record_evacuation("upload-2".to_string(), sharks.clone());
        tracker.record_evacuation("upload-3".to_string(), sharks);

        let mut count = 0;
        let mut seen_ids = std::collections::HashSet::new();

        for (upload_id, _sharks) in tracker.get_affected_uploads() {
            count += 1;
            seen_ids.insert(upload_id.clone());
        }

        assert_eq!(count, 3);
        assert_eq!(seen_ids.len(), 3);
        assert!(seen_ids.contains("upload-1"));
        assert!(seen_ids.contains("upload-2"));
        assert!(seen_ids.contains("upload-3"));
    }
}
