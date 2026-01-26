/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

//! Mdapi Discovery Module
//!
//! This module provides object discovery from buckets-mdapi for shark evacuation.
//! Unlike moray discovery which queries the `manta` table directly, mdapi discovery
//! must enumerate owners, their buckets, and then objects within those buckets.

use crate::SharkspotterMessage;
use libmanta::mdapi::{ListParams, MdapiClient};
use libmanta::moray::MantaObject;
use serde_json::{json, Value};
use slog::{debug, error, info, Logger};
use std::io::{Error, ErrorKind};
use uuid::Uuid;

/// Convert MantaObject to serde_json::Value for SharkspotterMessage
///
/// This ensures mdapi objects have the same format as moray objects,
/// with the addition of a bucket_id field.
pub fn manta_object_to_value(obj: &MantaObject, bucket_id: Uuid) -> Value {
    json!({
        "contentLength": obj.content_length,
        "contentMD5": obj.content_md5,
        "contentType": obj.content_type,
        "creator": obj.creator,
        "dirname": obj.dirname,
        "etag": obj.etag,
        "headers": obj.headers,
        "key": obj.key,
        "mtime": obj.mtime,
        "name": obj.name,
        "objectId": obj.object_id,
        "owner": obj.owner,
        "roles": obj.roles,
        "sharks": obj.sharks,
        "type": obj.obj_type,
        "vnode": obj.vnode,
        "bucket_id": bucket_id.to_string(),  // NEW: identifies bucket objects
    })
}

/// Check if object's sharks array contains any of the filter sharks
pub fn object_on_target_shark(
    sharks: &[libmanta::moray::MantaObjectShark],
    filter_sharks: &[String],
) -> bool {
    sharks.iter().any(|obj_shark| {
        filter_sharks
            .iter()
            .any(|filter| &obj_shark.manta_storage_id == filter)
    })
}

/// Check if an object value's sharks array contains any of the filter sharks
/// Returns the matching shark storage_id if found, None otherwise
fn value_on_target_shark(obj_value: &Value, filter_sharks: &[String]) -> Option<String> {
    if let Some(sharks) = obj_value.get("sharks").and_then(|s| s.as_array()) {
        for shark in sharks {
            if let Some(storage_id) = shark.get("manta_storage_id").and_then(|s| s.as_str()) {
                if filter_sharks.iter().any(|f| f == storage_id) {
                    return Some(storage_id.to_string());
                }
            }
        }
    }
    None
}

/// Discover bucket objects for a specific shard from mdapi
///
/// This function:
/// 1. For each owner in config.owners
/// 2. Lists all buckets for that owner on this vnode (shard)
/// 3. For each bucket, lists all objects
/// 4. Filters objects to only those on target sharks
/// 5. Converts to SharkspotterMessage and sends to channel
///
/// # Arguments
/// * `mdapi_client` - Connected mdapi client
/// * `owners` - List of owner UUIDs to query
/// * `shard` - Vnode/shard number to query
/// * `filter_sharks` - Only include objects on these sharks
/// * `obj_tx` - Channel to send discovered objects
/// * `log` - Logger
///
/// # Returns
/// * `Ok(usize)` - Number of objects discovered
/// * `Err(Error)` - If discovery fails
pub fn discover_mdapi_objects_for_shard(
    mdapi_client: &MdapiClient,
    owners: &[Uuid],
    shard: u32,
    filter_sharks: &[String],
    obj_tx: &crossbeam_channel::Sender<SharkspotterMessage>,
    log: &Logger,
) -> Result<usize, Error> {
    let mut total_objects = 0;

    info!(
        log,
        "Starting mdapi discovery for shard {} with {} owners",
        shard,
        owners.len()
    );

    for owner in owners {
        debug!(
            log,
            "Listing buckets for owner {} on shard {}", owner, shard
        );

        // List all buckets for this owner on this vnode
        let buckets = mdapi_client
            .list_buckets(*owner, shard as u64, None, 1000)
            .map_err(|e| {
                error!(
                    log,
                    "Failed to list buckets for owner {}: {}", owner, e
                );
                Error::new(ErrorKind::Other, e.to_string())
            })?;

        debug!(
            log,
            "Found {} buckets for owner {} on shard {}",
            buckets.len(),
            owner,
            shard
        );

        for bucket in buckets {
            debug!(
                log,
                "Listing objects in bucket {} ({})", bucket.name, bucket.id
            );

            // List objects in this bucket with pagination
            let mut marker: Option<String> = None;
            let mut bucket_object_count = 0;

            loop {
                let params = ListParams {
                    limit: 1000,
                    prefix: None,
                    marker: marker.clone(),
                };

                let objects = mdapi_client
                    .list_objects(*owner, bucket.id, params)
                    .map_err(|e| {
                        error!(
                            log,
                            "Failed to list objects in bucket {}: {}",
                            bucket.id,
                            e
                        );
                        Error::new(ErrorKind::Other, e.to_string())
                    })?;

                let object_count = objects.len();
                debug!(
                    log,
                    "Got {} objects from bucket {} (marker: {:?})",
                    object_count,
                    bucket.name,
                    marker
                );

                for obj_value in &objects {
                    // Check if object is on target shark using the raw Value
                    if let Some(matching_shark) = value_on_target_shark(obj_value, filter_sharks) {
                        // Get etag from the value
                        let etag = obj_value
                            .get("etag")
                            .and_then(|e| e.as_str())
                            .unwrap_or("")
                            .to_string();

                        // Add bucket_id to the value
                        let mut manta_value = obj_value.clone();
                        if let Some(obj) = manta_value.as_object_mut() {
                            obj.insert(
                                "bucket_id".to_string(),
                                Value::String(bucket.id.to_string()),
                            );
                        }

                        // Create SharkspotterMessage with the actual matching shark
                        let ss_msg = SharkspotterMessage {
                            manta_value,
                            etag,
                            shark: matching_shark,
                            shard,
                        };

                        // Send to channel
                        obj_tx.send(ss_msg).map_err(|e| {
                            error!(
                                log,
                                "Failed to send object to channel: {}", e
                            );
                            Error::new(ErrorKind::Other, e.to_string())
                        })?;

                        bucket_object_count += 1;
                        total_objects += 1;
                    }
                }

                // Check if there are more objects (pagination)
                if object_count < 1000 {
                    break;
                }

                // Update marker for next page - get name from the last object
                marker = objects
                    .last()
                    .and_then(|o| o.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string());
            }

            debug!(
                log,
                "Found {} objects on target shark in bucket {}",
                bucket_object_count,
                bucket.name
            );
        }
    }

    info!(
        log,
        "Mdapi discovery for shard {} complete: {} objects discovered",
        shard,
        total_objects
    );

    Ok(total_objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libmanta::moray::MantaObjectShark;

    #[test]
    fn test_object_on_target_shark() {
        let sharks = vec![
            MantaObjectShark {
                datacenter: "dc1".to_string(),
                manta_storage_id: "1.stor.domain".to_string(),
            },
            MantaObjectShark {
                datacenter: "dc1".to_string(),
                manta_storage_id: "2.stor.domain".to_string(),
            },
        ];

        let filter_sharks = vec!["1.stor.domain".to_string()];
        assert!(object_on_target_shark(&sharks, &filter_sharks));

        let filter_sharks = vec!["3.stor.domain".to_string()];
        assert!(!object_on_target_shark(&sharks, &filter_sharks));
    }

    #[test]
    fn test_manta_object_to_value_has_bucket_id() {
        let obj = MantaObject {
            content_length: 1024,
            content_md5: "abc123".to_string(),
            content_type: "text/plain".to_string(),
            creator: "test-user".to_string(),
            dirname: "/test".to_string(),
            etag: "etag123".to_string(),
            headers: serde_json::Value::Object(serde_json::Map::new()),
            key: "/test/file.txt".to_string(),
            mtime: 123456789,
            name: "file.txt".to_string(),
            object_id: "obj-123".to_string(),
            owner: "owner-uuid".to_string(),
            roles: vec![],
            sharks: vec![],
            vnode: 42,
            obj_type: "object".to_string(),
        };

        let bucket_id = Uuid::new_v4();
        let value = manta_object_to_value(&obj, bucket_id);

        assert!(value.get("bucket_id").is_some());
        assert_eq!(
            value.get("bucket_id").unwrap().as_str().unwrap(),
            bucket_id.to_string()
        );
    }

    #[test]
    fn test_value_on_target_shark() {
        let obj = json!({
            "sharks": [
                {"manta_storage_id": "1.stor.domain", "datacenter": "dc1"},
                {"manta_storage_id": "2.stor.domain", "datacenter": "dc1"}
            ]
        });

        // Should return the matching shark
        let filter_sharks = vec!["1.stor.domain".to_string()];
        let result = value_on_target_shark(&obj, &filter_sharks);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "1.stor.domain");

        // Should return the second shark when that's the filter
        let filter_sharks = vec!["2.stor.domain".to_string()];
        let result = value_on_target_shark(&obj, &filter_sharks);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "2.stor.domain");

        // Should return None when no match
        let filter_sharks = vec!["3.stor.domain".to_string()];
        assert!(value_on_target_shark(&obj, &filter_sharks).is_none());
    }
}
