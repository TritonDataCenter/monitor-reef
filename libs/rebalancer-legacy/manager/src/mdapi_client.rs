/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

//! Mdapi Client Module
//!
//! This module provides a client wrapper for the buckets-mdapi service,
//! which uses structured PostgreSQL tables instead of moray's flexible
//! JSON key-value storage. It handles schema translation between the
//! rebalancer's MantaObject format and mdapi's ObjectPayload format.
//!
//! The mdapi client provides equivalent functionality to the moray client
//! but works with the structured schema used by manta-buckets-api.
//!
//! # Backend Selection
//!
//! To use mdapi instead of moray in evacuate jobs:
//!
//! 1. Enable mdapi in config:
//!    ```toml
//!    [mdapi]
//!    enabled = true
//!    endpoint = "mdapi.example.com:2030"
//!    default_bucket_id = "550e8400-e29b-41d4-a716-446655440000"
//!    ```
//!
//! 2. In evacuate.rs, check config and use appropriate client:
//!    ```rust,ignore
//!    if job_config.mdapi.enabled {
//!        let mclient = mdapi_client::create_client(&job_config.mdapi.endpoint)?;
//!        let bucket_id = job_config.mdapi.default_bucket_id.unwrap();
//!        mdapi_client::put_object(&mclient, &object, bucket_id, Some(&etag))?;
//!    } else {
//!        let mut mclient = moray_client::create_client(shard, &domain)?;
//!        moray_client::put_object(&mut mclient, &object_value, &etag)?;
//!    }
//!    ```

use libmanta::mdapi::{
    Conditions, ListParams, MdapiClient, ObjectPayload, ObjectUpdate,
    StorageNodeIdentifier,
};
use libmanta::moray::{MantaObject, MantaObjectShark};
use rebalancer::error::{Error, InternalError, InternalErrorCode};
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

use crate::config::MdapiConfig;

// Vnode hash algorithm constants
// These match the buckets-mdapi vnode distribution algorithm
// Default algorithm uses MD5 hashing with standard interval
const DEFAULT_VNODE_HASH_INTERVAL: u128 = 0x1000000000000000000000000; // 2^96

/// Creates an mdapi client for the given endpoint.
///
/// Unlike moray which uses DNS SRV records, mdapi clients connect directly
/// to Fast RPC endpoints specified as "host:port" strings.
///
/// # Arguments
/// * `endpoint` - The mdapi service endpoint (e.g., "localhost:2030" or "mdapi.domain.com:2030")
///
/// # Returns
/// * `Result<MdapiClient, Error>` - The initialized client or error
///
/// # Errors
/// * Returns Error::Mdapi if the endpoint is invalid or connection fails
pub fn create_client(endpoint: &str) -> Result<MdapiClient, Error> {
    debug!("Creating mdapi client for endpoint: {}", endpoint);

    // Validate endpoint format (must contain port)
    if !endpoint.contains(':') {
        return Err(Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMdapiClient),
            format!(
                "Invalid mdapi endpoint format (missing port): {}",
                endpoint
            ),
        )));
    }

    // Create the mdapi client
    MdapiClient::new(endpoint).map_err(|e| {
        error!("Failed to create mdapi client for {}: {}", endpoint, e);
        Error::from(e)
    })
}

/// Converts a MantaObject (moray format) to ObjectPayload (mdapi format).
///
/// This function handles schema translation between moray's flexible JSON
/// storage and mdapi's structured PostgreSQL schema.
///
/// # Arguments
/// * `obj` - The MantaObject from moray
/// * `bucket_id` - The bucket UUID to associate with this object
/// * `request_id` - Optional request ID (generates new UUID if None)
///
/// # Returns
/// * `Result<ObjectPayload, Error>` - The translated object payload
pub fn manta_object_to_payload(
    obj: &MantaObject,
    bucket_id: Uuid,
    request_id: Option<Uuid>,
) -> Result<ObjectPayload, Error> {
    // Parse owner UUID from string
    let owner = Uuid::parse_str(&obj.owner).map_err(|e| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!("Invalid owner UUID: {}", e),
        ))
    })?;

    // Parse object ID from string
    let id = Uuid::parse_str(&obj.object_id).map_err(|e| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!("Invalid object_id UUID: {}", e),
        ))
    })?;

    // Convert vnode from i64 to u64
    let vnode = obj.vnode as u64;

    // Convert headers from JSON Value to HashMap
    let headers = match &obj.headers {
        Value::Object(map) => {
            let mut header_map = HashMap::new();
            for (key, value) in map {
                // Convert each header value to string
                let value_str = match value {
                    Value::String(s) => s.clone(),
                    _ => value.to_string(),
                };
                header_map.insert(key.clone(), value_str);
            }
            header_map
        }
        _ => HashMap::new(),
    };

    // Convert sharks from MantaObjectShark to StorageNodeIdentifier
    let sharks: Vec<StorageNodeIdentifier> = obj
        .sharks
        .iter()
        .map(|shark| StorageNodeIdentifier {
            datacenter: shark.datacenter.clone(),
            manta_storage_id: shark.manta_storage_id.clone(),
        })
        .collect();

    // Use provided request_id or generate a new one
    let req_id = request_id.unwrap_or_else(Uuid::new_v4);

    // Build conditions from etag if present
    let conditions = if !obj.etag.is_empty() {
        Some(Conditions {
            if_match: Some(vec![obj.etag.clone()]),
            if_none_match: None,
            if_modified_since: None,
            if_unmodified_since: None,
        })
    } else {
        None
    };

    Ok(ObjectPayload {
        owner,
        bucket_id,
        name: obj.name.clone(),
        id,
        vnode,
        content_length: obj.content_length,
        content_md5: obj.content_md5.clone(),
        content_type: obj.content_type.clone(),
        headers,
        sharks,
        properties: None,
        request_id: req_id,
        conditions,
    })
}

/// Converts an ObjectPayload (mdapi format) to MantaObject (moray format).
///
/// This function handles the reverse translation from mdapi's structured
/// PostgreSQL schema back to moray's JSON format.
///
/// # Arguments
/// * `payload` - The ObjectPayload from mdapi
///
/// # Returns
/// * `Result<MantaObject, Error>` - The translated MantaObject
pub fn payload_to_manta_object(
    payload: &ObjectPayload,
) -> Result<MantaObject, Error> {
    // Convert UUIDs to strings
    let owner = payload.owner.to_string();
    let object_id = payload.id.to_string();

    // Convert vnode from u64 to i64
    let vnode = payload.vnode as i64;

    // Convert headers from HashMap to JSON Value
    let headers_map: serde_json::Map<String, Value> = payload
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let headers = Value::Object(headers_map);

    // Convert sharks from StorageNodeIdentifier to MantaObjectShark
    let sharks: Vec<MantaObjectShark> = payload
        .sharks
        .iter()
        .map(|shark| MantaObjectShark {
            datacenter: shark.datacenter.clone(),
            manta_storage_id: shark.manta_storage_id.clone(),
        })
        .collect();

    // Extract etag from conditions if present
    let etag = payload
        .conditions
        .as_ref()
        .and_then(|c| c.if_match.as_ref())
        .and_then(|m| m.first())
        .cloned()
        .unwrap_or_default();

    Ok(MantaObject {
        headers,
        key: payload.name.clone(),
        mtime: 0, // Not provided by mdapi
        name: payload.name.clone(),
        creator: owner.clone(), // Use owner as creator
        dirname: String::new(), // Not provided by mdapi
        owner,
        roles: Vec::new(), // Not provided by mdapi
        vnode,
        content_length: payload.content_length,
        content_md5: payload.content_md5.clone(),
        content_type: payload.content_type.clone(),
        object_id,
        etag,
        sharks,
        obj_type: "object".to_string(),
    })
}

/// Find objects in a bucket using mdapi.
///
/// This function wraps mdapi's list_objects call and converts the results
/// to MantaObject format for compatibility with the rebalancer.
///
/// # Arguments
/// * `mclient` - The mdapi client instance
/// * `owner` - Owner account UUID
/// * `bucket_id` - Bucket UUID to query
/// * `prefix` - Optional prefix filter for object names
/// * `limit` - Maximum number of objects to return (1-1024)
///
/// # Returns
/// * `Result<Vec<MantaObject>, Error>` - List of objects in moray format
///
/// # Errors
/// * Returns Error::Mdapi on RPC failures or invalid parameters
pub fn find_objects(
    mclient: &MdapiClient,
    owner: Uuid,
    bucket_id: Uuid,
    prefix: Option<String>,
    limit: u32,
) -> Result<Vec<MantaObject>, Error> {
    trace!(
        "Finding objects: owner={}, bucket_id={}, prefix={:?}, limit={}",
        owner,
        bucket_id,
        prefix,
        limit
    );

    // Build list parameters
    let params = ListParams {
        limit,
        prefix,
        marker: None,
    };

    // Call mdapi list_objects
    let results = mclient.list_objects(owner, bucket_id, params)?;

    trace!("Found {} objects from mdapi", results.len());

    // Convert each JSON Value to ObjectPayload, then to MantaObject
    let mut objects = Vec::new();
    for value in results {
        // Deserialize JSON value to ObjectPayload
        let payload: ObjectPayload =
            serde_json::from_value(value).map_err(|e| {
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::BadMdapiClient),
                    format!("Failed to deserialize object payload: {}", e),
                ))
            })?;

        // Convert to MantaObject
        let manta_obj = payload_to_manta_object(&payload)?;
        objects.push(manta_obj);
    }

    debug!("Converted {} objects to MantaObject format", objects.len());
    Ok(objects)
}

/// Update an object in mdapi with conditional update support.
///
/// This function wraps mdapi's update_object call and handles the conversion
/// from MantaObject format to ObjectUpdate format, including etag-based
/// conditional updates.
///
/// # Arguments
/// * `mclient` - The mdapi client instance
/// * `object` - The MantaObject to update
/// * `bucket_id` - The bucket UUID containing the object
/// * `etag` - Optional etag for conditional update (if-match)
///
/// # Returns
/// * `Result<(), Error>` - Success or error
///
/// # Errors
/// * Returns Error::Mdapi on RPC failures
/// * Returns Error::Internal if object data is malformed
pub fn put_object(
    mclient: &MdapiClient,
    object: &MantaObject,
    bucket_id: Uuid,
    etag: Option<&str>,
) -> Result<(), Error> {
    // Parse owner UUID from object
    let owner = Uuid::parse_str(&object.owner).map_err(|e| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!("Invalid owner UUID: {}", e),
        ))
    })?;

    // Convert vnode from i64 to u64
    let vnode = object.vnode as u64;

    // Convert sharks from MantaObjectShark to StorageNodeIdentifier
    let sharks: Vec<StorageNodeIdentifier> = object
        .sharks
        .iter()
        .map(|shark| StorageNodeIdentifier {
            datacenter: shark.datacenter.clone(),
            manta_storage_id: shark.manta_storage_id.clone(),
        })
        .collect();

    // Convert headers from JSON Value to HashMap
    let headers = match &object.headers {
        Value::Object(map) => {
            let mut header_map = HashMap::new();
            for (key, value) in map {
                let value_str = match value {
                    Value::String(s) => s.clone(),
                    _ => value.to_string(),
                };
                header_map.insert(key.clone(), value_str);
            }
            header_map
        }
        _ => HashMap::new(),
    };

    // Build conditions from etag if provided
    let conditions = etag.map(|e| Conditions {
        if_match: Some(vec![e.to_string()]),
        if_none_match: None,
        if_modified_since: None,
        if_unmodified_since: None,
    });

    // Build ObjectUpdate payload
    let update = ObjectUpdate {
        owner,
        bucket_id,
        name: object.name.clone(),
        vnode,
        request_id: Uuid::new_v4(),
        sharks: Some(sharks),
        headers: Some(headers),
        properties: None,
        conditions,
    };

    trace!(
        "Updating object: name={}, vnode={}, etag={:?}",
        object.name,
        vnode,
        etag
    );

    // Call mdapi update_object
    mclient.update_object(update).map_err(|e| {
        error!("Failed to update object {}: {}", object.name, e);
        Error::from(e)
    })?;

    debug!("Successfully updated object: {}", object.name);
    Ok(())
}

/// Batch update multiple objects with error handling and fallback logic.
///
/// This function processes multiple object updates efficiently by grouping
/// them by vnode and handling errors gracefully. If individual updates fail,
/// they are logged but don't stop the entire batch.
///
/// # Arguments
/// * `mclient` - The mdapi client instance
/// * `objects` - Vector of (MantaObject, bucket_id, etag) tuples to update
///
/// # Returns
/// * `Result<BatchUpdateResult, Error>` - Summary of successful and failed updates
///
/// # Notes
/// Unlike moray's batch API, mdapi currently processes updates individually.
/// This function provides the same interface with optimized error handling.
pub struct BatchUpdateResult {
    pub successful: usize,
    pub failed: usize,
    pub errors: Vec<(String, Error)>, // (object_name, error)
}

pub fn batch_update(
    mclient: &MdapiClient,
    objects: Vec<(&MantaObject, Uuid, Option<&str>)>,
) -> Result<BatchUpdateResult, Error> {
    info!("Starting batch update of {} objects", objects.len());

    let mut successful = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    // Group objects by vnode for better cache locality
    // This allows mdapi server to handle updates to the same vnode more efficiently
    let mut grouped: HashMap<i64, Vec<(&MantaObject, Uuid, Option<&str>)>> =
        HashMap::new();
    for obj_tuple in objects {
        let vnode = obj_tuple.0.vnode;
        grouped
            .entry(vnode)
            .or_insert_with(Vec::new)
            .push(obj_tuple);
    }

    debug!(
        "Grouped {} objects into {} vnodes",
        grouped.values().map(|v| v.len()).sum::<usize>(),
        grouped.len()
    );

    // Process each vnode group
    for (vnode, vnode_objects) in grouped {
        trace!(
            "Processing vnode {} with {} objects",
            vnode,
            vnode_objects.len()
        );

        for (object, bucket_id, etag) in vnode_objects {
            match put_object(mclient, object, bucket_id, etag) {
                Ok(_) => {
                    successful += 1;
                    trace!("Successfully updated object: {}", object.name);
                }
                Err(e) => {
                    failed += 1;
                    warn!("Failed to update object {}: {}", object.name, e);
                    errors.push((object.name.clone(), e));
                }
            }
        }
    }

    info!(
        "Batch update complete: {} successful, {} failed",
        successful, failed
    );

    Ok(BatchUpdateResult {
        successful,
        failed,
        errors,
    })
}

/// Calculate vnode for an object based on owner, bucket, and object key.
///
/// This function implements the same vnode distribution algorithm used by
/// buckets-mdapi and buckets-mdplacement. The vnode is calculated by:
/// 1. Creating a composite key: "owner:bucket:object_key"
/// 2. Hashing the key using MD5
/// 3. Dividing the hash by VNODE_HASH_INTERVAL to get the vnode number
///
/// # Arguments
/// * `owner` - Owner UUID as string
/// * `bucket` - Bucket name or UUID as string
/// * `object_key` - Object name/key
///
/// # Returns
/// * `u64` - The calculated vnode number
///
/// # Notes
/// This matches the algorithm in manta-buckets-api lib/common.js getDataLocation()
pub fn calculate_vnode(owner: &str, bucket: &str, object_key: &str) -> u64 {
    // Create composite key in same format as buckets-mdapi
    let tkey = format!("{}:{}:{}", owner, bucket, object_key);

    trace!("Calculating vnode for key: {}", tkey);

    // Hash the key using MD5
    let hash_result = md5::compute(tkey.as_bytes());

    // Convert hash bytes to u128 (MD5 is 128 bits)
    let mut hash_value: u128 = 0;
    for (i, byte) in hash_result.iter().enumerate() {
        hash_value |= (*byte as u128) << (i * 8);
    }

    // Divide by vnode hash interval to get vnode
    let vnode = (hash_value / DEFAULT_VNODE_HASH_INTERVAL) as u64;

    trace!("Calculated vnode {} for key {}", vnode, tkey);

    vnode
}

/// Verify that an object's vnode matches the expected calculation.
///
/// This can be used to validate object metadata before updates.
///
/// # Arguments
/// * `object` - The MantaObject to verify
/// * `bucket` - Bucket identifier
///
/// # Returns
/// * `Result<bool, Error>` - true if vnode matches, false otherwise
pub fn verify_vnode(object: &MantaObject, bucket: &str) -> Result<bool, Error> {
    let calculated = calculate_vnode(&object.owner, bucket, &object.key);
    let stored = object.vnode as u64;

    if calculated != stored {
        warn!(
            "Vnode mismatch for object {}: calculated={}, stored={}",
            object.key, calculated, stored
        );
        Ok(false)
    } else {
        Ok(true)
    }
}

/// Check if mdapi backend should be used based on configuration.
///
/// This helper function encapsulates the logic for determining which
/// metadata backend (moray vs mdapi) should be used for operations.
///
/// # Arguments
/// * `config` - The MdapiConfig to check
///
/// # Returns
/// * `bool` - true if mdapi should be used, false for moray
///
/// # Example
/// ```rust,ignore
/// use crate::config::MdapiConfig;
/// use crate::mdapi_client;
///
/// let config = MdapiConfig {
///     enabled: true,
///     endpoint: "mdapi.example.com:2030".to_string(),
///     default_bucket_id: Some(uuid::Uuid::new_v4()),
///     connection_timeout_ms: 5000,
/// };
///
/// if mdapi_client::should_use_mdapi(&config) {
///     // Use mdapi_client functions
/// } else {
///     // Use moray_client functions
/// }
/// ```
pub fn should_use_mdapi(config: &MdapiConfig) -> bool {
    config.enabled && !config.endpoint.is_empty()
}

/// Information about a bucket returned by list_buckets
///
/// This struct holds essential bucket metadata needed for evacuation operations.
/// It matches the `Bucket` struct from rust-libmanta mdapi module.
#[derive(Debug, Clone)]
pub struct BucketInfo {
    /// Unique bucket identifier
    pub id: Uuid,
    /// Bucket name
    pub name: String,
    /// Owner account UUID
    pub owner: Uuid,
}

/// List all buckets for a given owner
///
/// Discovers all buckets in the mdapi deployment that belong to the specified
/// owner. This is used during evacuation to find all buckets that may contain
/// objects stored on the shark being evacuated.
///
/// # Arguments
/// * `client` - The mdapi client instance
/// * `owner` - Owner UUID to filter buckets
///
/// # Returns
/// * `Result<Vec<BucketInfo>, Error>` - List of buckets or error
///
/// # Errors
/// * Returns Error::Mdapi if the RPC call fails
/// * Returns Error::Internal for malformed responses
///
/// # Notes
/// The underlying rust-libmanta list_buckets is not yet fully implemented.
/// Currently returns an empty list with a debug log. When the mdapi Fast RPC
/// is completed, this will return actual bucket data.
///
/// # Example
/// ```rust,ignore
/// let client = mdapi_client::create_client("mdapi.example.com:2030")?;
/// let owner = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")?;
/// let buckets = mdapi_client::list_buckets(&client, owner)?;
///
/// for bucket in buckets {
///     println!("Bucket: {} ({})", bucket.name, bucket.id);
/// }
/// ```
pub fn list_buckets(
    client: &MdapiClient,
    owner: Uuid,
) -> Result<Vec<BucketInfo>, Error> {
    debug!("Discovering buckets for owner {} via mdapi", owner);

    // Call rust-libmanta's list_buckets
    // Query vnode 0 for buckets - in a complete implementation, we would
    // query all vnodes and aggregate results
    match client.list_buckets(owner, 0, None, 1000) {
        Ok(buckets) => {
            debug!("Discovered {} buckets", buckets.len());
            let bucket_infos = buckets
                .into_iter()
                .map(|b| BucketInfo {
                    id: b.id,
                    name: b.name,
                    owner: b.owner,
                })
                .collect();
            Ok(bucket_infos)
        }
        Err(e) => {
            // Check if it's the "Not implemented" error
            let error_msg = format!("{}", e);
            if error_msg.contains("Not implemented") {
                debug!(
                    "Mdapi list_buckets not yet implemented, returning empty list"
                );
                // Return empty list for now - evacuation will use default_bucket_id
                Ok(Vec::new())
            } else {
                error!("Failed to list buckets: {}", e);
                Err(Error::from(e))
            }
        }
    }
}

/// Get a single object by name with its full content.
///
/// This function retrieves an object's metadata and content, which is useful
/// for reading JSON-based metadata objects like MPU upload records. For regular
/// data objects, this would fetch the entire content which may be large.
///
/// # Arguments
/// * `mclient` - The mdapi client instance
/// * `owner` - The owner UUID
/// * `bucket_id` - The bucket UUID containing the object
/// * `object_name` - The object key/name to retrieve
///
/// # Returns
/// * `Result<(MantaObject, String), Error>` - Tuple of (object metadata, content)
///
/// # Errors
/// * Returns Error::Mdapi if the object doesn't exist or RPC fails
/// * Returns Error::Internal if object data is malformed
///
/// # Example
/// ```rust,ignore
/// let (object, content) = get_object_with_content(
///     &client,
///     owner,
///     bucket_id,
///     ".mpu-uploads/abc-123"
/// )?;
/// let upload_record: serde_json::Value = serde_json::from_str(&content)?;
/// ```
///
/// # Performance
/// Cost: O(1) single RPC call to mdapi. For large objects, consider streaming.
pub fn get_object_with_content(
    mclient: &MdapiClient,
    owner: Uuid,
    bucket_id: Uuid,
    object_name: &str,
) -> Result<(MantaObject, String), Error> {
    trace!(
        "Getting object with content: owner={}, bucket_id={}, name={}",
        owner,
        bucket_id,
        object_name
    );

    // Calculate vnode for this object
    let vnode = calculate_vnode(&owner.to_string(), &bucket_id.to_string(), object_name);

    // Get object from mdapi
    // Note: rust-libmanta's get_object returns ObjectPayload which includes
    // content for small objects. For MPU upload records (JSON metadata),
    // the content will be included.
    let value = mclient.get_object(owner, bucket_id, object_name, vnode)?;

    // Deserialize to ObjectPayload
    let payload: ObjectPayload = serde_json::from_value(value).map_err(|e| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMdapiClient),
            format!("Failed to deserialize object payload: {}", e),
        ))
    })?;

    // Extract content if present
    // For metadata objects like upload records, content is stored in the payload
    let content = if let Some(ref props) = payload.properties {
        if let Some(content_value) = props.get("content") {
            // Content may be stored as a string or bytes
            if let Some(s) = content_value.as_str() {
                s.to_string()
            } else {
                // If it's not a string, serialize it as JSON
                serde_json::to_string(content_value).map_err(|e| {
                    Error::Internal(InternalError::new(
                        Some(InternalErrorCode::BadMdapiClient),
                        format!("Failed to serialize content: {}", e),
                    ))
                })?
            }
        } else {
            String::new()
        }
    } else {
        // No properties or content field - return empty string
        String::new()
    };

    // Convert payload to MantaObject
    let manta_obj = payload_to_manta_object(&payload)?;

    debug!("Retrieved object {} with {} bytes of content", object_name, content.len());
    Ok((manta_obj, content))
}

/// Update an object's content while preserving its metadata.
///
/// This function is specifically designed for updating JSON-based metadata
/// objects like MPU upload records. It updates the content field while
/// preserving all other object metadata (sharks, etag, headers, etc.).
///
/// # Arguments
/// * `mclient` - The mdapi client instance
/// * `owner` - The owner UUID
/// * `bucket_id` - The bucket UUID containing the object
/// * `object_name` - The object key/name to update
/// * `new_content` - The new content to store (typically JSON string)
///
/// # Returns
/// * `Result<(), Error>` - Success or error
///
/// # Errors
/// * Returns Error::Mdapi if the object doesn't exist or RPC fails
/// * Returns Error::Internal if object data is malformed
///
/// # Example
/// ```rust,ignore
/// // Update MPU upload record with new shark locations
/// let (object, content) = get_object_with_content(
///     &client, owner, bucket_id, ".mpu-uploads/abc-123"
/// )?;
/// let mut record: serde_json::Value = serde_json::from_str(&content)?;
/// record["preAllocatedSharks"] = serde_json::to_value(&new_sharks)?;
/// let updated_content = serde_json::to_string(&record)?;
///
/// update_object_content(
///     &client, owner, bucket_id, ".mpu-uploads/abc-123", &updated_content
/// )?;
/// ```
///
/// # Invariants
/// - Object metadata (sharks, headers, etag) is preserved
/// - Only the content field is updated
/// - Content must be valid UTF-8 string
///
/// # Performance
/// Cost: O(1) single RPC call to mdapi. Updates are atomic.
pub fn update_object_content(
    mclient: &MdapiClient,
    owner: Uuid,
    bucket_id: Uuid,
    object_name: &str,
    new_content: &str,
) -> Result<(), Error> {
    trace!(
        "Updating object content: owner={}, bucket_id={}, name={}, content_len={}",
        owner,
        bucket_id,
        object_name,
        new_content.len()
    );

    // Calculate vnode for this object
    let vnode = calculate_vnode(&owner.to_string(), &bucket_id.to_string(), object_name);

    // First, get the current object to preserve its metadata
    let (current_object, _old_content) = get_object_with_content(
        mclient,
        owner,
        bucket_id,
        object_name,
    )?;

    // Build properties value containing the new content.
    // The read path (get_object_with_content) expects
    // properties = {"content": <value>}, so wrap accordingly.
    let content_value = match serde_json::from_str::<Value>(new_content) {
        Ok(v) => v,
        Err(e) => {
            error!(
                "Failed to parse new_content as JSON for {}: {}",
                object_name, e
            );
            return Err(Error::from(e));
        }
    };
    let properties = json!({ "content": content_value });

    let request_id = Uuid::new_v4();
    let payload = manta_object_to_payload(&current_object, bucket_id, Some(request_id))?;

    // Create ObjectUpdate with properties carrying the content update.
    // Use an etag condition to prevent lost updates from concurrent writes.
    let update = ObjectUpdate {
        name: object_name.to_string(),
        vnode,
        owner,
        bucket_id,
        request_id,
        sharks: Some(payload.sharks.clone()),
        headers: Some(payload.headers.clone()),
        properties: Some(properties),
        conditions: Some(Conditions {
            if_match: Some(vec![current_object.etag.clone()]),
            if_none_match: None,
            if_modified_since: None,
            if_unmodified_since: None,
        }),
    };

    // Perform the update
    mclient.update_object(update).map_err(|e| {
        error!("Failed to update object content for {}: {}", object_name, e);
        Error::from(e)
    })?;

    debug!("Successfully updated content for object: {}", object_name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazy_static::lazy_static;
    use rebalancer::util;
    use serde_json::json;
    use std::sync::Mutex;

    lazy_static! {
        // Ensure logger is initialized for tests that use logging macros
        static ref TEST_LOGGER_GUARD: Mutex<Option<slog_scope::GlobalLoggerGuard>> = {
            let guard = util::init_global_logger(None);
            Mutex::new(Some(guard))
        };
    }

    // Call this to ensure the logger is initialized before tests that log
    fn ensure_logger_initialized() {
        // Just access the lazy_static to trigger initialization
        let _guard = TEST_LOGGER_GUARD.lock();
    }

    // Helper to create a test MantaObject
    fn create_test_manta_object() -> MantaObject {
        MantaObject {
            headers: json!({
                "content-disposition": "attachment",
                "x-custom-header": "test-value"
            }),
            key: "/user/stor/test-object.txt".to_string(),
            mtime: 1234567890,
            name: "test-object.txt".to_string(),
            creator: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            dirname: "/user/stor".to_string(),
            owner: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            roles: vec![],
            vnode: 42,
            content_length: 1024,
            content_md5: "rL0Y20zC+Fzt72VPzMSk2A==".to_string(),
            content_type: "text/plain".to_string(),
            object_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            etag: "abc123def456".to_string(),
            sharks: vec![
                MantaObjectShark {
                    datacenter: "us-east-1".to_string(),
                    manta_storage_id: "1.stor.example.com".to_string(),
                },
                MantaObjectShark {
                    datacenter: "us-west-1".to_string(),
                    manta_storage_id: "2.stor.example.com".to_string(),
                },
            ],
            obj_type: "object".to_string(),
        }
    }

    #[test]
    fn test_manta_object_to_payload_basic() {
        let manta_obj = create_test_manta_object();
        let bucket_id =
            Uuid::parse_str("999e8400-e29b-41d4-a716-446655440099").unwrap();
        let request_id =
            Uuid::parse_str("111e8400-e29b-41d4-a716-446655440011").unwrap();

        let result =
            manta_object_to_payload(&manta_obj, bucket_id, Some(request_id));

        assert!(result.is_ok());
        let payload = result.unwrap();

        assert_eq!(payload.owner.to_string(), manta_obj.owner);
        assert_eq!(payload.bucket_id, bucket_id);
        assert_eq!(payload.name, manta_obj.name);
        assert_eq!(payload.id.to_string(), manta_obj.object_id);
        assert_eq!(payload.vnode, 42);
        assert_eq!(payload.content_length, 1024);
        assert_eq!(payload.content_md5, manta_obj.content_md5);
        assert_eq!(payload.content_type, "text/plain");
        assert_eq!(payload.request_id, request_id);
    }

    #[test]
    fn test_manta_object_to_payload_headers_conversion() {
        let manta_obj = create_test_manta_object();
        let bucket_id = Uuid::new_v4();

        let result = manta_object_to_payload(&manta_obj, bucket_id, None);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert_eq!(payload.headers.len(), 2);
        assert_eq!(
            payload.headers.get("content-disposition").unwrap(),
            "attachment"
        );
        assert_eq!(
            payload.headers.get("x-custom-header").unwrap(),
            "test-value"
        );
    }

    #[test]
    fn test_manta_object_to_payload_sharks_conversion() {
        let manta_obj = create_test_manta_object();
        let bucket_id = Uuid::new_v4();

        let result = manta_object_to_payload(&manta_obj, bucket_id, None);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert_eq!(payload.sharks.len(), 2);
        assert_eq!(payload.sharks[0].datacenter, "us-east-1");
        assert_eq!(payload.sharks[0].manta_storage_id, "1.stor.example.com");
        assert_eq!(payload.sharks[1].datacenter, "us-west-1");
        assert_eq!(payload.sharks[1].manta_storage_id, "2.stor.example.com");
    }

    #[test]
    fn test_manta_object_to_payload_etag_to_conditions() {
        let manta_obj = create_test_manta_object();
        let bucket_id = Uuid::new_v4();

        let result = manta_object_to_payload(&manta_obj, bucket_id, None);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert!(payload.conditions.is_some());
        let conditions = payload.conditions.unwrap();
        assert!(conditions.if_match.is_some());
        assert_eq!(conditions.if_match.unwrap()[0], "abc123def456");
    }

    #[test]
    fn test_payload_to_manta_object_basic() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id = Uuid::new_v4();
        let object_id =
            Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/plain".to_string());

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "test.txt".to_string(),
            id: object_id,
            vnode: 42,
            content_length: 512,
            content_md5: "test-md5==".to_string(),
            content_type: "text/plain".to_string(),
            headers,
            sharks: vec![StorageNodeIdentifier {
                datacenter: "dc1".to_string(),
                manta_storage_id: "stor1.example.com".to_string(),
            }],
            properties: None,
            request_id: Uuid::new_v4(),
            conditions: Some(Conditions {
                if_match: Some(vec!["etag123".to_string()]),
                if_none_match: None,
                if_modified_since: None,
                if_unmodified_since: None,
            }),
        };

        let result = payload_to_manta_object(&payload);
        assert!(result.is_ok());

        let manta_obj = result.unwrap();
        assert_eq!(manta_obj.owner, owner.to_string());
        assert_eq!(manta_obj.name, "test.txt");
        assert_eq!(manta_obj.object_id, object_id.to_string());
        assert_eq!(manta_obj.vnode, 42);
        assert_eq!(manta_obj.content_length, 512);
        assert_eq!(manta_obj.etag, "etag123");
        assert_eq!(manta_obj.sharks.len(), 1);
        assert_eq!(manta_obj.sharks[0].datacenter, "dc1");
    }

    #[test]
    fn test_round_trip_conversion() {
        let original = create_test_manta_object();
        let bucket_id = Uuid::new_v4();

        // Convert MantaObject -> ObjectPayload
        let payload_result =
            manta_object_to_payload(&original, bucket_id, None);
        assert!(payload_result.is_ok());
        let payload = payload_result.unwrap();

        // Convert ObjectPayload -> MantaObject
        let manta_result = payload_to_manta_object(&payload);
        assert!(manta_result.is_ok());
        let round_trip = manta_result.unwrap();

        // Verify key fields match
        assert_eq!(round_trip.owner, original.owner);
        assert_eq!(round_trip.name, original.name);
        assert_eq!(round_trip.object_id, original.object_id);
        assert_eq!(round_trip.vnode, original.vnode);
        assert_eq!(round_trip.content_length, original.content_length);
        assert_eq!(round_trip.content_md5, original.content_md5);
        assert_eq!(round_trip.content_type, original.content_type);
        assert_eq!(round_trip.etag, original.etag);
        assert_eq!(round_trip.sharks.len(), original.sharks.len());
    }

    #[test]
    fn test_calculate_vnode_consistency() {
        ensure_logger_initialized();
        let owner = "550e8400-e29b-41d4-a716-446655440000";
        let bucket = "test-bucket";
        let key = "test-object.txt";

        // Calculate vnode multiple times - should be consistent
        let vnode1 = calculate_vnode(owner, bucket, key);
        let vnode2 = calculate_vnode(owner, bucket, key);
        let vnode3 = calculate_vnode(owner, bucket, key);

        assert_eq!(vnode1, vnode2);
        assert_eq!(vnode2, vnode3);
    }

    #[test]
    fn test_calculate_vnode_different_keys() {
        ensure_logger_initialized();
        let owner = "550e8400-e29b-41d4-a716-446655440000";
        let bucket = "test-bucket";

        let vnode1 = calculate_vnode(owner, bucket, "object1.txt");
        let vnode2 = calculate_vnode(owner, bucket, "object2.txt");

        // Different keys should (usually) produce different vnodes
        // Note: There's a small chance of collision, but unlikely with MD5
        assert_ne!(vnode1, vnode2);
    }

    #[test]
    fn test_verify_vnode_matches() {
        ensure_logger_initialized();
        let owner = "550e8400-e29b-41d4-a716-446655440000";
        let bucket = "test-bucket";
        let key = "test.txt";

        let calculated_vnode = calculate_vnode(owner, bucket, key);

        let mut test_obj = create_test_manta_object();
        test_obj.owner = owner.to_string();
        test_obj.key = key.to_string();
        test_obj.vnode = calculated_vnode as i64;

        let result = verify_vnode(&test_obj, bucket);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), true);
    }

    #[test]
    fn test_verify_vnode_mismatch() {
        ensure_logger_initialized();
        let bucket = "test-bucket";

        let mut test_obj = create_test_manta_object();
        test_obj.vnode = 99999; // Wrong vnode

        let result = verify_vnode(&test_obj, bucket);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false);
    }

    // Client operation tests

    #[test]
    fn test_create_client_valid_endpoint() {
        ensure_logger_initialized();
        let result = create_client("localhost:2030");
        // Should succeed in creating client (even though RPC won't work)
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_client_invalid_endpoint_no_port() {
        ensure_logger_initialized();
        let result = create_client("localhost");
        // Should fail - missing port
        assert!(result.is_err());

        if let Err(Error::Internal(e)) = result {
            assert_eq!(e.code, InternalErrorCode::BadMdapiClient);
            let error_msg = format!("{}", Error::Internal(e.clone()));
            assert!(error_msg.contains("missing port"));
        } else {
            panic!("Expected Internal error with BadMdapiClient");
        }
    }

    #[test]
    fn test_create_client_with_domain() {
        ensure_logger_initialized();
        let result = create_client("mdapi.example.com:2030");
        assert!(result.is_ok());
    }

    #[test]
    fn test_batch_update_result_structure() {
        // Test BatchUpdateResult structure
        let result = BatchUpdateResult {
            successful: 5,
            failed: 2,
            errors: vec![(
                "obj1".to_string(),
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::Other),
                    "test error".to_string(),
                )),
            )],
        };

        assert_eq!(result.successful, 5);
        assert_eq!(result.failed, 2);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].0, "obj1");
    }

    #[test]
    fn test_batch_update_empty_list() {
        ensure_logger_initialized();
        let client = create_client("localhost:2030").unwrap();
        let objects: Vec<(&MantaObject, Uuid, Option<&str>)> = vec![];

        let result = batch_update(&client, objects);
        assert!(result.is_ok());

        let batch_result = result.unwrap();
        assert_eq!(batch_result.successful, 0);
        assert_eq!(batch_result.failed, 0);
        assert_eq!(batch_result.errors.len(), 0);
    }

    #[test]
    fn test_manta_object_invalid_owner_uuid() {
        let mut manta_obj = create_test_manta_object();
        manta_obj.owner = "invalid-uuid".to_string();

        let bucket_id = Uuid::new_v4();
        let result = manta_object_to_payload(&manta_obj, bucket_id, None);

        assert!(result.is_err());
        if let Err(Error::Internal(e)) = result {
            assert_eq!(e.code, InternalErrorCode::BadMantaObject);
            let error_msg = format!("{}", Error::Internal(e.clone()));
            assert!(error_msg.contains("Invalid owner UUID"));
        } else {
            panic!("Expected Internal error with BadMantaObject");
        }
    }

    #[test]
    fn test_manta_object_invalid_object_id() {
        let mut manta_obj = create_test_manta_object();
        manta_obj.object_id = "not-a-uuid".to_string();

        let bucket_id = Uuid::new_v4();
        let result = manta_object_to_payload(&manta_obj, bucket_id, None);

        assert!(result.is_err());
        if let Err(Error::Internal(e)) = result {
            assert_eq!(e.code, InternalErrorCode::BadMantaObject);
            let error_msg = format!("{}", Error::Internal(e.clone()));
            assert!(error_msg.contains("Invalid object_id UUID"));
        } else {
            panic!("Expected Internal error with BadMantaObject");
        }
    }

    #[test]
    fn test_manta_object_empty_etag() {
        let mut manta_obj = create_test_manta_object();
        manta_obj.etag = String::new();

        let bucket_id = Uuid::new_v4();
        let result = manta_object_to_payload(&manta_obj, bucket_id, None);

        assert!(result.is_ok());
        let payload = result.unwrap();
        // Empty etag should result in no conditions
        assert!(payload.conditions.is_none());
    }

    #[test]
    fn test_payload_headers_empty() {
        let owner = Uuid::new_v4();
        let bucket_id = Uuid::new_v4();
        let object_id = Uuid::new_v4();

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "test.txt".to_string(),
            id: object_id,
            vnode: 42,
            content_length: 100,
            content_md5: "md5".to_string(),
            content_type: "text/plain".to_string(),
            headers: HashMap::new(), // Empty headers
            sharks: vec![],
            properties: None,
            request_id: Uuid::new_v4(),
            conditions: None,
        };

        let result = payload_to_manta_object(&payload);
        assert!(result.is_ok());

        let manta_obj = result.unwrap();
        // Should have empty JSON object for headers
        assert_eq!(manta_obj.headers, json!({}));
    }

    #[test]
    fn test_payload_no_conditions() {
        let owner = Uuid::new_v4();
        let bucket_id = Uuid::new_v4();
        let object_id = Uuid::new_v4();

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "test.txt".to_string(),
            id: object_id,
            vnode: 42,
            content_length: 100,
            content_md5: "md5".to_string(),
            content_type: "text/plain".to_string(),
            headers: HashMap::new(),
            sharks: vec![],
            properties: None,
            request_id: Uuid::new_v4(),
            conditions: None, // No conditions
        };

        let result = payload_to_manta_object(&payload);
        assert!(result.is_ok());

        let manta_obj = result.unwrap();
        // Should have empty etag
        assert_eq!(manta_obj.etag, "");
    }

    #[test]
    fn test_vnode_type_conversion_i64_to_u64() {
        let manta_obj = create_test_manta_object();
        assert_eq!(manta_obj.vnode, 42); // i64

        let bucket_id = Uuid::new_v4();
        let result = manta_object_to_payload(&manta_obj, bucket_id, None);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert_eq!(payload.vnode, 42); // u64
    }

    #[test]
    fn test_vnode_type_conversion_u64_to_i64() {
        let owner = Uuid::new_v4();
        let bucket_id = Uuid::new_v4();
        let object_id = Uuid::new_v4();

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "test.txt".to_string(),
            id: object_id,
            vnode: 12345_u64,
            content_length: 100,
            content_md5: "md5".to_string(),
            content_type: "text/plain".to_string(),
            headers: HashMap::new(),
            sharks: vec![],
            properties: None,
            request_id: Uuid::new_v4(),
            conditions: None,
        };

        let result = payload_to_manta_object(&payload);
        assert!(result.is_ok());

        let manta_obj = result.unwrap();
        assert_eq!(manta_obj.vnode, 12345_i64);
    }

    #[test]
    fn test_headers_with_non_string_json_values() {
        let mut manta_obj = create_test_manta_object();
        manta_obj.headers = json!({
            "string-header": "value",
            "number-header": 42,
            "bool-header": true,
            "array-header": ["a", "b"]
        });

        let bucket_id = Uuid::new_v4();
        let result = manta_object_to_payload(&manta_obj, bucket_id, None);

        assert!(result.is_ok());
        let payload = result.unwrap();

        // Non-string values should be converted to strings
        assert_eq!(payload.headers.get("string-header").unwrap(), "value");
        assert_eq!(payload.headers.get("number-header").unwrap(), "42");
        assert_eq!(payload.headers.get("bool-header").unwrap(), "true");
        assert!(payload.headers.get("array-header").unwrap().contains("a"));
    }

    #[test]
    fn test_calculate_vnode_deterministic() {
        ensure_logger_initialized();
        // Same inputs should always produce same vnode
        for _ in 0..10 {
            let vnode = calculate_vnode(
                "550e8400-e29b-41d4-a716-446655440000",
                "bucket",
                "key",
            );
            assert_eq!(
                vnode,
                calculate_vnode(
                    "550e8400-e29b-41d4-a716-446655440000",
                    "bucket",
                    "key"
                )
            );
        }
    }

    #[test]
    fn test_should_use_mdapi_enabled() {
        let config = MdapiConfig {
            enabled: true,
            endpoint: "mdapi.example.com:2030".to_string(),
            default_bucket_id: Some(Uuid::new_v4()),
            connection_timeout_ms: 5000,
            single_bucket_mode: false,
        };

        assert_eq!(should_use_mdapi(&config), true);
    }

    #[test]
    fn test_should_use_mdapi_disabled() {
        let config = MdapiConfig {
            enabled: false,
            endpoint: "mdapi.example.com:2030".to_string(),
            default_bucket_id: Some(Uuid::new_v4()),
            connection_timeout_ms: 5000,
            single_bucket_mode: false,
        };

        assert_eq!(should_use_mdapi(&config), false);
    }

    #[test]
    fn test_should_use_mdapi_empty_endpoint() {
        let config = MdapiConfig {
            enabled: true,
            endpoint: String::new(),
            default_bucket_id: Some(Uuid::new_v4()),
            connection_timeout_ms: 5000,
            single_bucket_mode: false,
        };

        assert_eq!(should_use_mdapi(&config), false);
    }

    #[test]
    fn test_should_use_mdapi_default_config() {
        let config = MdapiConfig::default();
        // Default config should prefer moray (enabled = false)
        assert_eq!(should_use_mdapi(&config), false);
    }

    // =========================================================================
    // Additional tests for batch_update and vnode grouping logic
    // =========================================================================

    #[test]
    fn test_batch_update_result_all_success() {
        let result = BatchUpdateResult {
            successful: 10,
            failed: 0,
            errors: vec![],
        };

        assert_eq!(result.successful, 10);
        assert_eq!(result.failed, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_batch_update_result_all_failed() {
        let result = BatchUpdateResult {
            successful: 0,
            failed: 5,
            errors: vec![
                (
                    "obj1".to_string(),
                    Error::Internal(InternalError::new(
                        Some(InternalErrorCode::Other),
                        "error 1".to_string(),
                    )),
                ),
                (
                    "obj2".to_string(),
                    Error::Internal(InternalError::new(
                        Some(InternalErrorCode::Other),
                        "error 2".to_string(),
                    )),
                ),
            ],
        };

        assert_eq!(result.successful, 0);
        assert_eq!(result.failed, 5);
        assert_eq!(result.errors.len(), 2);
    }

    #[test]
    fn test_batch_update_result_partial_failure() {
        let result = BatchUpdateResult {
            successful: 8,
            failed: 2,
            errors: vec![(
                "failed_obj".to_string(),
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::MetadataUpdateFailure),
                    "update failed".to_string(),
                )),
            )],
        };

        assert_eq!(result.successful, 8);
        assert_eq!(result.failed, 2);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].0.contains("failed_obj"));
    }

    #[test]
    fn test_vnode_calculation_consistency() {
        ensure_logger_initialized();
        // Same input should always produce same vnode
        let owner = "550e8400-e29b-41d4-a716-446655440000";
        let bucket = "660e8400-e29b-41d4-a716-446655440001";
        let key = "/user/stor/test.txt";

        let vnode1 = calculate_vnode(owner, bucket, key);
        let vnode2 = calculate_vnode(owner, bucket, key);

        assert_eq!(vnode1, vnode2);
    }

    #[test]
    fn test_vnode_calculation_different_keys() {
        ensure_logger_initialized();
        let owner = "550e8400-e29b-41d4-a716-446655440000";
        let bucket = "660e8400-e29b-41d4-a716-446655440001";

        let vnode1 = calculate_vnode(owner, bucket, "file1.txt");
        let vnode2 = calculate_vnode(owner, bucket, "file2.txt");

        // Different keys should produce different vnodes (with high probability)
        // Note: There's a small chance of collision, but very unlikely
        assert_ne!(vnode1, vnode2);
    }

    #[test]
    fn test_vnode_calculation_different_owners() {
        ensure_logger_initialized();
        let bucket = "660e8400-e29b-41d4-a716-446655440001";
        let key = "test.txt";

        let vnode1 = calculate_vnode("owner1", bucket, key);
        let vnode2 = calculate_vnode("owner2", bucket, key);

        assert_ne!(vnode1, vnode2);
    }

    #[test]
    fn test_vnode_calculation_different_buckets() {
        ensure_logger_initialized();
        let owner = "550e8400-e29b-41d4-a716-446655440000";
        let key = "test.txt";

        let vnode1 = calculate_vnode(owner, "bucket1", key);
        let vnode2 = calculate_vnode(owner, "bucket2", key);

        assert_ne!(vnode1, vnode2);
    }

    #[test]
    fn test_vnode_calculation_empty_strings() {
        ensure_logger_initialized();
        // Edge case: empty strings should still produce valid vnode
        let vnode = calculate_vnode("", "", "");
        // Should not panic and should return some value
        // Vnode range is 0 to 2^32 (MD5 128bit / 2^96 hash interval)
        assert!(vnode <= u32::MAX as u64);
    }

    #[test]
    fn test_vnode_calculation_special_characters() {
        ensure_logger_initialized();
        let owner = "550e8400-e29b-41d4-a716-446655440000";
        let bucket = "660e8400-e29b-41d4-a716-446655440001";

        // Keys with special characters
        let vnode1 = calculate_vnode(owner, bucket, "/user/uploads/.mpu-parts/abc-123/0");
        let vnode2 = calculate_vnode(owner, bucket, "file with spaces.txt");
        let vnode3 = calculate_vnode(owner, bucket, "日本語ファイル.txt");

        // All should produce valid vnodes (within u32 range due to hash interval)
        assert!(vnode1 <= u32::MAX as u64);
        assert!(vnode2 <= u32::MAX as u64);
        assert!(vnode3 <= u32::MAX as u64);
    }

    #[test]
    fn test_verify_vnode_matching() {
        ensure_logger_initialized();
        let manta_obj = create_test_manta_object();
        let bucket_id = Uuid::new_v4();
        let bucket_str = bucket_id.to_string();

        // Calculate expected vnode using the key field (not name)
        let expected_vnode = calculate_vnode(
            &manta_obj.owner,
            &bucket_str,
            &manta_obj.key,
        );

        // Create object with correct vnode
        let mut correct_obj = manta_obj.clone();
        correct_obj.vnode = expected_vnode as i64;

        let result = verify_vnode(&correct_obj, &bucket_str);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_verify_vnode_mismatched() {
        ensure_logger_initialized();
        let mut manta_obj = create_test_manta_object();
        let bucket_id = Uuid::new_v4();
        let bucket_str = bucket_id.to_string();

        // Set a definitely wrong vnode
        manta_obj.vnode = 9999;

        let result = verify_vnode(&manta_obj, &bucket_str);
        assert!(result.is_ok());
        // The function returns false for mismatched vnodes
        assert!(!result.unwrap());
    }

    #[test]
    fn test_manta_object_to_payload_preserves_fields() {
        let manta_obj = create_test_manta_object();
        let bucket_id = Uuid::new_v4();

        let result = manta_object_to_payload(&manta_obj, bucket_id, None);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert_eq!(payload.name, manta_obj.name);
        assert_eq!(payload.content_length, manta_obj.content_length as u64);
        assert_eq!(payload.content_md5, manta_obj.content_md5);
        assert_eq!(payload.content_type, manta_obj.content_type);
        assert_eq!(payload.bucket_id, bucket_id);
        assert_eq!(payload.sharks.len(), manta_obj.sharks.len());
    }

    #[test]
    fn test_manta_object_with_multiple_sharks() {
        let mut manta_obj = create_test_manta_object();
        manta_obj.sharks = vec![
            libmanta::moray::MantaObjectShark {
                datacenter: "us-east-1".to_string(),
                manta_storage_id: "1.stor.domain".to_string(),
            },
            libmanta::moray::MantaObjectShark {
                datacenter: "us-west-2".to_string(),
                manta_storage_id: "2.stor.domain".to_string(),
            },
            libmanta::moray::MantaObjectShark {
                datacenter: "eu-central-1".to_string(),
                manta_storage_id: "3.stor.domain".to_string(),
            },
        ];

        let bucket_id = Uuid::new_v4();
        let result = manta_object_to_payload(&manta_obj, bucket_id, None);
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert_eq!(payload.sharks.len(), 3);
        assert_eq!(payload.sharks[0].datacenter, "us-east-1");
        assert_eq!(payload.sharks[1].datacenter, "us-west-2");
        assert_eq!(payload.sharks[2].datacenter, "eu-central-1");
    }

    #[test]
    fn test_manta_object_with_request_id() {
        let manta_obj = create_test_manta_object();
        let bucket_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();

        // With request_id provided
        let result = manta_object_to_payload(&manta_obj, bucket_id, Some(request_id));
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert_eq!(payload.request_id, request_id);
    }

    #[test]
    fn test_manta_object_without_request_id() {
        let manta_obj = create_test_manta_object();
        let bucket_id = Uuid::new_v4();

        // Without request_id (generates one internally)
        let result = manta_object_to_payload(&manta_obj, bucket_id, None);
        assert!(result.is_ok());

        let payload = result.unwrap();
        // Should have a request_id even when not provided
        assert!(!payload.request_id.is_nil());
    }
}
