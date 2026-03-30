/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2026 Edgecast Cloud LLC.
 */

//! Mdapi Client Module
//!
//! Provides a client wrapper for the buckets-mdapi service,
//! which uses structured PostgreSQL tables instead of moray's
//! flexible JSON key-value storage.  Handles schema translation
//! between the rebalancer's MantaObject format and mdapi's
//! ObjectPayload format.
//!
//! The mdapi client provides equivalent functionality to the
//! moray client but works with the structured schema used by
//! manta-buckets-api.
//!
//! # DNS SRV Discovery
//!
//! Like the moray client, mdapi uses DNS SRV records for
//! service discovery.  The `create_client` function resolves
//! `_buckets-mdapi._tcp.{host}` to discover the IP and port.
//!
//! # Backend Selection
//!
//! To use mdapi instead of moray in evacuate jobs:
//!
//! 1. Configure mdapi shards (populated via
//!    BUCKETS_MORAY_SHARDS SAPI metadata):
//!    ```json
//!    "mdapi": {
//!        "shards": [
//!            {"host": "1.buckets-mdapi.coal.joyent.us"},
//!            {"host": "2.buckets-mdapi.coal.joyent.us"}
//!        ]
//!    }
//!    ```
//!
//! 2. In evacuate.rs, check config and use appropriate
//!    client:
//!    ```rust,ignore
//!    if mdapi_client::should_use_mdapi(&job_config.mdapi) {
//!        let mclient =
//!            mdapi_client::create_client(&shard_host)?;
//!        mdapi_client::put_object(
//!            &mclient, &object, bucket_id, Some(&etag),
//!        )?;
//!    } else {
//!        let mut mclient =
//!            moray_client::create_client(shard, &domain)?;
//!        moray_client::put_object(
//!            &mut mclient, &object_value, &etag,
//!        )?;
//!    }
//!    ```

use libmanta::mdapi::{
    Conditions, ListParams, MdapiClient, MdapiError, ObjectPayload,
    ObjectUpdate, StorageNodeIdentifier,
};
use libmanta::moray::{MantaObject, MantaObjectShark};
use rand::seq::SliceRandom;
use rebalancer::error::{Error, InternalError, InternalErrorCode};
use resolve::resolve_host;
use resolve::{record::Srv, DnsConfig, DnsResolver};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::net::{IpAddr, SocketAddr};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

use crate::config::MdapiConfig;

// Vnode hash algorithm constants
// These match the buckets-mdapi vnode distribution algorithm
// Default algorithm uses MD5 hashing with standard interval
const DEFAULT_VNODE_HASH_INTERVAL: u128 = 0x1000000000000000000000000; // 2^96

/// Resolve a DNS SRV record for a given service, protocol,
/// and host.  Returns one randomly chosen SRV record.
fn get_srv_record(
    svc: &str,
    proto: &str,
    host: &str,
) -> Result<Srv, Error> {
    let query = format!("{}.{}.{}", svc, proto, host);
    let r = DnsResolver::new(DnsConfig::load_default()?)?;
    r.resolve_record::<Srv>(&query)?
        .choose(&mut rand::thread_rng())
        .map(|r| r.to_owned())
        .ok_or_else(|| {
            InternalError::new(
                Some(InternalErrorCode::IpLookupError),
                format!(
                    "mdapi SRV lookup returned 0 results for {}",
                    query
                ),
            )
            .into()
        })
}

/// Resolve a hostname to a single IP address.
fn lookup_ip(host: &str) -> Result<IpAddr, Error> {
    match resolve_host(host)?
        .collect::<Vec<IpAddr>>()
        .first()
    {
        Some(a) => Ok(*a),
        None => Err(InternalError::new(
            Some(InternalErrorCode::IpLookupError),
            format!(
                "mdapi IP lookup returned 0 results for {}",
                host
            ),
        )
        .into()),
    }
}

/// Resolve the `_buckets-mdapi._tcp.{host}` SRV record and
/// return a SocketAddr with the resolved IP and port.
pub fn get_mdapi_srv_sockaddr(
    host: &str,
) -> Result<SocketAddr, Error> {
    let srv_record =
        get_srv_record("_buckets-mdapi", "_tcp", host)?;
    let ip = lookup_ip(&srv_record.target)?;

    Ok(SocketAddr::new(ip, srv_record.port))
}

/// Creates an mdapi client for the given hostname.
///
/// Resolves `_buckets-mdapi._tcp.{host}` via DNS SRV to
/// discover the IP and port, then connects to the resolved
/// endpoint.  This mirrors how moray_client discovers the
/// moray service via `_moray._tcp`.
///
/// # Arguments
/// * `host` - The mdapi shard hostname
///   (e.g., "1.buckets-mdapi.coal.joyent.us")
///
/// # Returns
/// * `Result<MdapiClient, Error>` - The initialized client
///
/// # Errors
/// * Returns error if SRV lookup or connection fails
pub fn create_client(host: &str) -> Result<MdapiClient, Error> {
    debug!("Creating mdapi client for host: {}", host);

    let sock_addr = get_mdapi_srv_sockaddr(host)?;
    trace!(
        "Resolved SRV for mdapi host {}: {}",
        host,
        sock_addr
    );

    let endpoint = format!("{}:{}", sock_addr.ip(), sock_addr.port());
    MdapiClient::new(&endpoint).map_err(|e| {
        error!(
            "Failed to create mdapi client for {}: {}",
            host, e
        );
        Error::from(e)
    })
}

/// Convert a JSON Value representing HTTP headers into a HashMap.
///
/// Returns an empty map for `null` (legitimate: object has no headers).
/// Logs a warning and returns an empty map for non-object types (data
/// corruption).
fn convert_headers(headers: &Value, object_label: &str) -> HashMap<String, String> {
    match headers {
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
        Value::Null => HashMap::new(),
        other => {
            warn!(
                "Object {} has malformed headers (expected JSON object, \
                 got {}); treating as empty",
                object_label, other
            );
            HashMap::new()
        }
    }
}

/// Convert a slice of MantaObjectShark into StorageNodeIdentifier vec.
fn convert_sharks(sharks: &[MantaObjectShark]) -> Vec<StorageNodeIdentifier> {
    sharks
        .iter()
        .map(|shark| StorageNodeIdentifier {
            datacenter: shark.datacenter.clone(),
            manta_storage_id: shark.manta_storage_id.clone(),
        })
        .collect()
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

    // Convert vnode from i64 to u64 (negative vnodes are invalid)
    // negative vnodes should not exists a vnode where the object lands is 
    // vnode = Md5(key of the object) / 2^96 
    let vnode = u64::try_from(obj.vnode).map_err(|_| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!("Negative vnode {} in object {}", obj.vnode, obj.object_id),
        ))
    })?;

    let headers = convert_headers(&obj.headers, &obj.object_id);
    let sharks = convert_sharks(&obj.sharks);

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

    // Convert vnode from u64 to i64 (overflow would be data corruption)
    let vnode = i64::try_from(payload.vnode).map_err(|_| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!(
                "Vnode {} exceeds i64::MAX for object {}",
                payload.vnode, payload.id
            ),
        ))
    })?;

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

    // Convert vnode from i64 to u64 safely
    let vnode = u64::try_from(object.vnode).map_err(|_| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!(
                "Negative vnode {} in object {}",
                object.vnode, object.key
            ),
        ))
    })?;

    let sharks = convert_sharks(&object.sharks);
    let headers = convert_headers(&object.headers, &object.key);

    // Parse object UUID
    let id = Uuid::parse_str(&object.object_id).map_err(|e| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!("Invalid object_id UUID: {}", e),
        ))
    })?;

    // Build conditions from etag if provided
    let conditions = match etag {
        Some(e) => Conditions {
            if_match: Some(vec![e.to_string()]),
            if_none_match: None,
            if_modified_since: None,
            if_unmodified_since: None,
        },
        None => Conditions::default(),
    };

    // Build ObjectUpdate payload
    let update = ObjectUpdate {
        owner,
        bucket_id,
        name: object.name.clone(),
        id,
        vnode,
        content_type: object.content_type.clone(),
        headers,
        properties: None,
        request_id: Uuid::new_v4(),
        sharks: Some(sharks),
        conditions,
    };

    info!(
        "mdapi put_object: name={}, id={}, vnode={}, bucket_id={}, \
         sharks={:?}, etag={:?}",
        update.name,
        update.id,
        update.vnode,
        update.bucket_id,
        update.sharks,
        etag
    );

    // Call mdapi update_object
    match mclient.update_object(update) {
        Ok(resp) => {
            info!(
                "mdapi put_object success: name={}, response={:?}",
                object.name, resp
            );
            Ok(())
        }
        Err(e) => {
            error!(
                "mdapi put_object failed: name={}, id={}, vnode={}, \
                 bucket_id={}, error={}",
                object.name, object.object_id, vnode, bucket_id, e
            );
            Err(Error::from(e))
        }
    }
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
/// When the server supports the batchupdateobjects RPC MANTA-5503, all
/// objects are sent in a single RPC and the server executes per-vnode
/// atomic transactions. For older servers, falls back to individual
/// put_object calls with vnode grouping for cache locality.
/// The result includes both successful and failed object IDs to enable
/// proper partial failure handling - callers can mark successful objects
/// as complete and only retry/error the failed ones.
/// Object IDs (UUIDs) are used instead of names to avoid ambiguity when
/// the same name exists across different buckets or vnodes.
pub struct BatchUpdateResult {
    pub successful: usize,
    pub failed: usize,
    pub successful_objects: Vec<String>, // object_id UUIDs (as strings) that succeeded
    pub errors: Vec<(String, Error)>,    // (object_id, error) for failures
}

/// Default maximum batch size if not configured
pub const DEFAULT_MAX_BATCH_SIZE: usize = 100;

/// Configuration for retry behavior with exponential backoff
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (0 = no retries)
    pub max_retries: u32,
    /// Initial delay between retries in milliseconds
    pub initial_backoff_ms: u64,
    /// Maximum delay between retries in milliseconds
    pub max_backoff_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        RetryConfig {
            max_retries: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 5000,
        }
    }
}

impl From<&MdapiConfig> for RetryConfig {
    fn from(config: &MdapiConfig) -> Self {
        RetryConfig {
            max_retries: config.max_retries,
            initial_backoff_ms: config.initial_backoff_ms,
            max_backoff_ms: config.max_backoff_ms,
        }
    }
}

/// Calculate the next backoff delay using exponential backoff.
///
/// The delay doubles with each attempt, capped at max_backoff_ms.
/// Formula: min(initial_backoff_ms * 2^attempt, max_backoff_ms)
pub fn calculate_backoff(
    attempt: u32,
    initial_backoff_ms: u64,
    max_backoff_ms: u64,
) -> Duration {
    // Prevent overflow by capping the exponent
    let exponent = attempt.min(30);
    let backoff_ms = initial_backoff_ms.saturating_mul(1u64 << exponent);
    Duration::from_millis(backoff_ms.min(max_backoff_ms))
}

/// Execute a fallible operation with exponential backoff retry.
///
/// This function will retry the operation up to `max_retries` times
/// with exponentially increasing delays between attempts.
///
/// # Arguments
/// * `retry_config` - Configuration for retry behavior
/// * `operation` - A closure that returns `Result<T, Error>`
///
/// # Returns
/// * `Result<T, Error>` - Success on first successful attempt, or last error if all retries fail
///
/// # Example
/// ```ignore
/// let config = RetryConfig::default();
/// let result = with_retry(&config, || {
///     put_object(&client, &obj, bucket_id, Some(&etag))
/// });
/// ```
pub fn with_retry<T, F>(retry_config: &RetryConfig, mut operation: F) -> Result<T, Error>
where
    F: FnMut() -> Result<T, Error>,
{
    let mut last_error = None;

    for attempt in 0..=retry_config.max_retries {
        match operation() {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt < retry_config.max_retries {
                    let backoff = calculate_backoff(
                        attempt,
                        retry_config.initial_backoff_ms,
                        retry_config.max_backoff_ms,
                    );
                    warn!(
                        "Operation failed (attempt {}/{}), retrying in {:?}: {}",
                        attempt + 1,
                        retry_config.max_retries + 1,
                        backoff,
                        e
                    );
                    thread::sleep(backoff);
                }
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::Other),
            "Retry exhausted with no error captured".to_string(),
        ))
    }))
}

/// Check if an error is retryable.
///
/// Transient errors (network issues, timeouts, server overload) are retryable.
/// Permanent errors (invalid data, not found, permission denied) are not.
pub fn is_retryable_error(error: &Error) -> bool {
    match error {
        Error::Internal(internal) => {
            // Client errors and generic errors may be transient
            matches!(
                internal.code,
                InternalErrorCode::BadMorayClient
                    | InternalErrorCode::BadMdapiClient
                    | InternalErrorCode::Other
            )
        }
        Error::Reqwest(_) => true, // Network errors are retryable
        Error::Hyper(_) => true,   // HTTP errors may be transient
        Error::IoError(_) => true, // I/O errors may be transient
        Error::Mdapi(mdapi_err) => {
            use libmanta::mdapi::MdapiError;
            // Transient mdapi errors (EAGAIN, connection reset, etc.)
            // are retryable.  Permanent errors (not found, precondition
            // failed, bad data) are not.
            matches!(
                mdapi_err,
                MdapiError::IoError(_)
                    | MdapiError::RpcError(_)
                    | MdapiError::DatabaseError(_)
                    | MdapiError::Other(_)
            )
        }
        Error::SerdeJson(_) => false, // Serialization errors are not retryable
        _ => false,
    }
}

/// Execute an operation with retry only for retryable errors.
///
/// Unlike `with_retry`, this version checks if the error is retryable
/// before attempting another retry.
pub fn with_retry_if_retryable<T, F>(
    retry_config: &RetryConfig,
    mut operation: F,
) -> Result<T, Error>
where
    F: FnMut() -> Result<T, Error>,
{
    let mut last_error = None;

    for attempt in 0..=retry_config.max_retries {
        match operation() {
            Ok(result) => return Ok(result),
            Err(e) => {
                if !is_retryable_error(&e) {
                    // Non-retryable error, fail immediately
                    return Err(e);
                }

                if attempt < retry_config.max_retries {
                    let backoff = calculate_backoff(
                        attempt,
                        retry_config.initial_backoff_ms,
                        retry_config.max_backoff_ms,
                    );
                    warn!(
                        "Retryable error (attempt {}/{}), retrying in {:?}: {}",
                        attempt + 1,
                        retry_config.max_retries + 1,
                        backoff,
                        e
                    );
                    thread::sleep(backoff);
                }
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::Other),
            "Retry exhausted with no error captured".to_string(),
        ))
    }))
}

/// Batch update multiple objects with automatic chunking for large batches.
///
/// If the batch exceeds `max_batch_size`, it is automatically split into
/// smaller chunks to prevent overloading the mdapi server.
///
/// # Arguments
/// * `mclient` - The mdapi client instance
/// * `objects` - Vector of (MantaObject, bucket_id, etag) tuples to update
/// * `max_batch_size` - Maximum objects per batch (uses DEFAULT_MAX_BATCH_SIZE if None)
///
/// # Returns
/// * `Result<BatchUpdateResult, Error>` - Summary of successful and failed updates
pub fn batch_update_with_config(
    mclient: &MdapiClient,
    objects: Vec<(&MantaObject, Uuid, Option<&str>)>,
    max_batch_size: Option<usize>,
) -> Result<BatchUpdateResult, Error> {
    let max_size = max_batch_size.unwrap_or(DEFAULT_MAX_BATCH_SIZE);
    let total_objects = objects.len();

    if total_objects == 0 {
        return Ok(BatchUpdateResult {
            successful: 0,
            failed: 0,
            successful_objects: Vec::new(),
            errors: Vec::new(),
        });
    }

    // If batch is within limits, process directly
    if total_objects <= max_size {
        return batch_update_internal(mclient, objects);
    }

    // Chunk large batches
    info!(
        "Batch of {} objects exceeds max_batch_size ({}), chunking into {} batches",
        total_objects,
        max_size,
        (total_objects + max_size - 1) / max_size
    );

    let mut total_successful = 0;
    let mut total_failed = 0;
    let mut all_successful_objects = Vec::new();
    let mut all_errors = Vec::new();

    for (chunk_idx, chunk) in objects.chunks(max_size).enumerate() {
        debug!(
            "Processing chunk {}/{} with {} objects",
            chunk_idx + 1,
            (total_objects + max_size - 1) / max_size,
            chunk.len()
        );

        // Convert chunk slice to owned Vec for processing
        let chunk_vec: Vec<(&MantaObject, Uuid, Option<&str>)> =
            chunk.iter().cloned().collect();

        match batch_update_internal(mclient, chunk_vec) {
            Ok(result) => {
                total_successful += result.successful;
                total_failed += result.failed;
                all_successful_objects.extend(result.successful_objects);
                all_errors.extend(result.errors);
            }
            Err(e) => {
                // If the entire chunk fails, mark all objects as failed
                error!("Chunk {} failed entirely: {}", chunk_idx + 1, e);
                total_failed += chunk.len();
                for (obj, _, _) in chunk {
                    all_errors.push((
                        obj.object_id.clone(),
                        Error::Internal(InternalError::new(
                            Some(InternalErrorCode::MetadataUpdateFailure),
                            format!("Chunk failed: {}", e),
                        )),
                    ));
                }
            }
        }
    }

    info!(
        "Chunked batch update complete: {} successful, {} failed across {} chunks",
        total_successful,
        total_failed,
        (total_objects + max_size - 1) / max_size
    );

    Ok(BatchUpdateResult {
        successful: total_successful,
        failed: total_failed,
        successful_objects: all_successful_objects,
        errors: all_errors,
    })
}

/// Original batch_update function for backward compatibility.
/// Uses DEFAULT_MAX_BATCH_SIZE for chunking.
pub fn batch_update(
    mclient: &MdapiClient,
    objects: Vec<(&MantaObject, Uuid, Option<&str>)>,
) -> Result<BatchUpdateResult, Error> {
    batch_update_with_config(mclient, objects, Some(DEFAULT_MAX_BATCH_SIZE))
}

/// Internal batch update using native batchupdateobjects
/// RPC with fallback to individual put_object calls.
///
/// # Strategy
///
/// 1. Convert objects to ObjectUpdate payloads.
/// 2. Send a single batchupdateobjects RPC.
/// 3. Parse per-object results from the response.
/// 4. If the server does not support the RPC (older
///    buckets-mdapi), fall back to individual calls.
///
/// # Time complexity
///
/// Native path: O(N) payload build + 1 RPC round-trip.
/// Fallback path: O(N) individual RPCs.
fn batch_update_internal(
    mclient: &MdapiClient,
    objects: Vec<(&MantaObject, Uuid, Option<&str>)>,
) -> Result<BatchUpdateResult, Error> {
    debug!("Processing batch of {} objects", objects.len());

    // Build ObjectUpdate payloads for all objects,
    // keeping an object_id index for result mapping.
    let mut updates = Vec::with_capacity(objects.len());
    let mut object_ids: Vec<String> =
        Vec::with_capacity(objects.len());

    for (object, bucket_id, etag) in &objects {
        let owner =
            Uuid::parse_str(&object.owner).map_err(|e| {
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::BadMantaObject),
                    format!("Invalid owner UUID: {}", e),
                ))
            })?;

        let vnode =
            u64::try_from(object.vnode).map_err(|_| {
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::BadMantaObject),
                    format!(
                        "Negative vnode {} in object {}",
                        object.vnode, object.key
                    ),
                ))
            })?;

        let id = Uuid::parse_str(&object.object_id)
            .map_err(|e| {
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::BadMantaObject),
                    format!(
                        "Invalid object_id UUID: {}",
                        e
                    ),
                ))
            })?;

        let sharks = convert_sharks(&object.sharks);
        let headers =
            convert_headers(&object.headers, &object.key);

        let conditions = match etag {
            Some(e) => Conditions {
                if_match: Some(vec![e.to_string()]),
                if_none_match: None,
                if_modified_since: None,
                if_unmodified_since: None,
            },
            None => Conditions::default(),
        };

        updates.push(libmanta::mdapi::ObjectUpdate {
            owner,
            bucket_id: *bucket_id,
            name: object.name.clone(),
            id,
            vnode,
            content_type: object.content_type.clone(),
            headers,
            properties: None,
            request_id: Uuid::new_v4(),
            sharks: Some(sharks),
            conditions,
        });

        object_ids.push(object.object_id.clone());
    }

    // Attempt native batch RPC
    match mclient.batch_update_objects(updates) {
        Ok(response) => {
            parse_batch_response(response, &object_ids)
        }
        Err(libmanta::mdapi::MdapiError::RpcError(
            ref msg,
        )) if msg.contains("Unsupported")
            || msg.contains("not implemented")
            || msg.contains("Not implemented") =>
        {
            info!(
                "batchupdateobjects RPC not supported, \
                 falling back to individual updates"
            );
            batch_update_fallback(mclient, objects)
        }
        Err(e) => {
            error!(
                "batchupdateobjects RPC failed: {}",
                e
            );
            Err(Error::from(e))
        }
    }
}

/// Parse the batchupdateobjects RPC response into a
/// BatchUpdateResult.
///
/// The response JSON has the shape:
/// ```json
/// {
///   "results": [
///   "failed_vnodes": [
///     { "vnode": V,
///       "error": { ... },
///       "objects": [ <original UpdateObjectPayload>, ... ] }
///   ]
/// }
/// ```
/// An empty `failed_vnodes` array means all objects
/// succeeded.  Each entry in `failed_vnodes[].objects` is
/// the full original request payload, so callers can
/// resubmit directly.
fn parse_batch_response(
    response: Value,
    object_ids: &[String],
) -> Result<BatchUpdateResult, Error> {
    let mut errors: Vec<(String, Error)> = Vec::new();

    // Collect per-vnode failures.
    if let Some(vnodes) = response
        .get("failed_vnodes")
        .and_then(|v| v.as_array())
    {
        for vf in vnodes {
            let vnode = vf
                .get("vnode")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let err_msg = vf
                .get("error")
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string());

            if let Some(objs) =
                vf.get("objects").and_then(|v| v.as_array())
            {
                for obj in objs {
                    let id = obj
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    errors.push((
                        id,
                        Error::Internal(InternalError::new(
                            Some(
                                InternalErrorCode::MetadataUpdateFailure,
                            ),
                            format!(
                                "vnode {} failed: {}",
                                vnode, err_msg
                            ),
                        )),
                    ));
                }
            }
        }
    }

    // Everything not in failed_vnodes succeeded.
    let failed_ids: std::collections::HashSet<&str> =
        errors.iter().map(|(id, _)| id.as_str()).collect();
    let successful_objects: Vec<String> = object_ids
        .iter()
        .filter(|id| !failed_ids.contains(id.as_str()))
        .cloned()
        .collect();

    let successful = successful_objects.len();
    let failed = errors.len();

    debug!(
        "Batch RPC complete: {} successful, {} failed",
        successful, failed
    );

    Ok(BatchUpdateResult {
        successful,
        failed,
        successful_objects,
        errors,
    })
}

/// Fallback: individual put_object calls when the server
/// does not support batchupdateobjects.
///
/// This preserves the original per-object behavior with
/// vnode grouping for cache locality.
fn batch_update_fallback(
    mclient: &MdapiClient,
    objects: Vec<(&MantaObject, Uuid, Option<&str>)>,
) -> Result<BatchUpdateResult, Error> {
    let mut successful = 0;
    let mut failed = 0;
    let mut successful_objects = Vec::new();
    let mut errors = Vec::new();

    // Group by vnode for cache locality
    let mut grouped: HashMap<
        i64,
        Vec<(&MantaObject, Uuid, Option<&str>)>,
    > = HashMap::new();
    for obj_tuple in objects {
        let vnode = obj_tuple.0.vnode;
        grouped
            .entry(vnode)
            .or_insert_with(Vec::new)
            .push(obj_tuple);
    }

    for (_vnode, vnode_objects) in grouped {
        for (object, bucket_id, etag) in vnode_objects {
            match put_object(
                mclient, object, bucket_id, etag,
            ) {
                Ok(_) => {
                    successful += 1;
                    successful_objects
                        .push(object.object_id.clone());
                }
                Err(e) => {
                    failed += 1;
                    warn!(
                        "Failed to update object {} ({}): {}",
                        object.object_id, object.name, e
                    );
                    errors
                        .push((object.object_id.clone(), e));
                }
            }
        }
    }

    debug!(
        "Fallback batch complete: {} successful, \
         {} failed",
        successful, failed
    );

    Ok(BatchUpdateResult {
        successful,
        failed,
        successful_objects,
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

    // Divide by vnode hash interval to get vnode.
    // 2^128 is MD5 hash generated (32 hex chars x 4 bit per char)
    // 2^96 is the DEFAULT_VNODE_HASH_INTERVAL
    // Result is at most 2^128 / 2^96 = 2^32, which fits in u64,
    // but use try_from for defense-in-depth.

    let vnode = u64::try_from(hash_value / DEFAULT_VNODE_HASH_INTERVAL)
        .expect("vnode exceeds u64::MAX; hash interval invariant violated");

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
    let stored = u64::try_from(object.vnode).map_err(|_| {
        Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            format!(
                "Negative vnode {} in object {}",
                object.vnode, object.key
            ),
        ))
    })?;

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
/// Returns true when the `shards` vec in `MdapiConfig` is non-empty,
/// meaning at least one `BUCKETS_MORAY_SHARDS` entry was configured.
///
/// # Arguments
/// * `config` - The MdapiConfig to check
///
/// # Returns
/// * `bool` - true if mdapi should be used, false for moray
///
/// # Example
/// ```rust,ignore
/// use crate::config::{MdapiConfig, MdapiShard};
/// use crate::mdapi_client;
///
/// let config = MdapiConfig {
///     shards: vec![MdapiShard { host: "1.buckets-mdapi.domain:2030".into() }],
///     connection_timeout_ms: 5000,
///     ..Default::default()
/// };
///
/// if mdapi_client::should_use_mdapi(&config) {
///     // Use mdapi_client functions
/// } else {
///     // Use moray_client functions
/// }
/// ```
pub fn should_use_mdapi(config: &MdapiConfig) -> bool {
    !config.shards.is_empty()
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
/// The underlying rust-libmanta list_buckets is fully implemented.
/// Returns bucket data from the mdapi service. Older servers that don't
/// support listBuckets RPC will return empty list as fallback.
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

    // Discover all vnodes, then query each for this owner's buckets.
    // Dedup by bucket id since the same bucket may appear on
    // multiple vnodes.
    let vnodes = list_vnodes(client)?;

    // If no vnodes discovered (server doesn't support the RPC),
    // fall back to querying vnode 0 only.
    let query_vnodes: Vec<u64> = if vnodes.is_empty() {
        vec![0]
    } else {
        vnodes
    };

    let mut seen = std::collections::HashSet::new();
    let mut all_buckets = Vec::new();

    for vnode in &query_vnodes {
        match client.list_buckets(owner, *vnode, None, 1000) {
            Ok(buckets) => {
                for b in buckets {
                    if seen.insert(b.id) {
                        all_buckets.push(BucketInfo {
                            id: b.id,
                            name: b.name,
                            owner: b.owner,
                        });
                    }
                }
            }
            Err(MdapiError::RpcError(ref msg))
                if msg.contains("Not implemented")
                    || msg.contains("not implemented")
                    || msg.contains("Unsupported") =>
            {
                debug!(
                    "Mdapi listBuckets RPC not supported, \
                     returning empty list"
                );
                return Ok(Vec::new());
            }
            Err(e) => {
                error!(
                    "Failed to list buckets on vnode {}: {}",
                    vnode, e
                );
                return Err(Error::from(e));
            }
        }
    }

    debug!(
        "Discovered {} buckets across {} vnodes",
        all_buckets.len(),
        query_vnodes.len()
    );
    Ok(all_buckets)
}

/// List all vnodes present on the mdapi instance.
///
/// Returns the vnode numbers from the `listvnodes` RPC. If the
/// server does not support this RPC, returns an empty vector
/// so callers can fall back to a default.
///
/// # Arguments
/// * `client` - The mdapi client instance
///
/// # Returns
/// * `Result<Vec<u64>, Error>` - Vnode numbers or error
///
/// # Algorithmic cost
/// O(1) single RPC round-trip.
pub fn list_vnodes(
    client: &MdapiClient,
) -> Result<Vec<u64>, Error> {
    debug!("Discovering vnodes via listvnodes RPC");

    match client.list_vnodes() {
        Ok(resp) => {
            debug!("Discovered {} vnodes", resp.vnodes.len());
            Ok(resp.vnodes)
        }
        Err(MdapiError::RpcError(ref msg))
            if msg.contains("Not implemented")
                || msg.contains("not implemented")
                || msg.contains("Unsupported") =>
        {
            debug!(
                "listvnodes RPC not supported, \
                 returning empty list"
            );
            Ok(Vec::new())
        }
        Err(e) => {
            error!("listvnodes RPC failed: {}", e);
            Err(Error::from(e))
        }
    }
}

/// List distinct owners on a specific vnode.
///
/// Returns owner UUIDs from the `listowners` RPC. If the server
/// does not support this RPC, returns an empty vector so callers
/// can fall back to configured owners.
///
/// # Arguments
/// * `client` - The mdapi client instance
/// * `vnode` - Vnode number to query
///
/// # Returns
/// * `Result<Vec<Uuid>, Error>` - Owner UUIDs or error
///
/// # Algorithmic cost
/// O(1) single RPC round-trip.
pub fn list_owners(
    client: &MdapiClient,
    vnode: u64,
) -> Result<Vec<Uuid>, Error> {
    debug!(
        "Discovering owners on vnode {} via listowners RPC",
        vnode
    );

    match client.list_owners(vnode) {
        Ok(resp) => {
            debug!(
                "Discovered {} owners on vnode {}",
                resp.owners.len(),
                vnode
            );
            Ok(resp.owners)
        }
        Err(MdapiError::RpcError(ref msg))
            if msg.contains("Not implemented")
                || msg.contains("not implemented")
                || msg.contains("Unsupported") =>
        {
            debug!(
                "listowners RPC not supported, \
                 returning empty list"
            );
            Ok(Vec::new())
        }
        Err(e) => {
            error!(
                "listowners RPC failed for vnode {}: {}",
                vnode, e
            );
            Err(Error::from(e))
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
/// Cost: O(1) single RPC call to mdapi
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
    let vnode = calculate_vnode(&owner.to_string(), 
                &bucket_id.to_string(), object_name);

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
/// .mpu objects hold the mpu parts metadata, for example in which sharks
/// does a part lives, so we need to update this to reflect where the .mpu-parts
/// live now, this means that mpu transfers will survive an evacuation as the parts
/// now point to their new location.
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
    let vnode = calculate_vnode(&owner.to_string(), 
        &bucket_id.to_string(), 
        object_name);

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
        owner,
        bucket_id,
        name: object_name.to_string(),
        id: payload.id,
        vnode,
        content_type: payload.content_type.clone(),
        headers: payload.headers.clone(),
        properties: Some(properties),
        request_id,
        sharks: Some(payload.sharks.clone()),
        conditions: Conditions {
            if_match: Some(vec![current_object.etag.clone()]),
            if_none_match: None,
            if_modified_since: None,
            if_unmodified_since: None,
        },
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
    use crate::config::MdapiShard;
    use rebalancer::util;
    use serde_json::json;

    /// Initialize a logger for the current test and return a guard that must be held
    /// for the duration of the test. This ensures each test has its own logger
    /// context, avoiding race conditions with parallel test execution.
    fn init_test_logger() -> slog_scope::GlobalLoggerGuard {
        // Use a discard logger to avoid noisy test output
        // Use Warning level to minimize overhead while still testing log paths
        use slog::{Drain, Logger, o};
        use std::sync::Mutex;

        let log = Logger::root(
            Mutex::new(slog::Discard).fuse(),
            o!("test" => true),
        );
        slog_scope::set_global_logger(log)
    }

    /// Create an MdapiClient for testing without DNS SRV lookup.
    ///
    /// Uses `MdapiClient::new` directly with a loopback address,
    /// bypassing `create_client` which requires live DNS.  The
    /// client is valid for tests that exercise serialization,
    /// empty-list handling, or batch-chunking logic — none of
    /// which send RPC traffic.
    fn create_test_client() -> MdapiClient {
        MdapiClient::new("127.0.0.1:2030")
            .expect("test client with loopback address")
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
        let _log_guard = init_test_logger();
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
        let _log_guard = init_test_logger();
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
        let _log_guard = init_test_logger();
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
        let _log_guard = init_test_logger();
        let bucket = "test-bucket";

        let mut test_obj = create_test_manta_object();
        test_obj.vnode = 99999; // Wrong vnode

        let result = verify_vnode(&test_obj, bucket);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false);
    }

    // Client operation tests

    /// Verify MdapiClient can be created with a valid host:port
    /// endpoint (bypasses DNS SRV, tests MdapiClient::new directly).
    #[test]
    fn test_create_client_valid_endpoint() {
        let _log_guard = init_test_logger();
        let result = MdapiClient::new("127.0.0.1:2030");
        assert!(result.is_ok());
    }

    /// Verify that create_client rejects a hostname with no SRV
    /// record by returning an appropriate error.
    #[test]
    fn test_create_client_unresolvable_host() {
        let _log_guard = init_test_logger();
        let result =
            create_client("no-such-host.invalid:2030");
        assert!(result.is_err());
    }

    /// Verify MdapiClient can be created with a domain:port
    /// endpoint (bypasses DNS SRV, tests MdapiClient::new directly).
    #[test]
    fn test_create_client_with_domain() {
        let _log_guard = init_test_logger();
        let result = MdapiClient::new("mdapi.example.com:2030");
        assert!(result.is_ok());
    }

    #[test]
    fn test_batch_update_result_structure() {
        // Test BatchUpdateResult structure (uses object_id UUIDs)
        let id1 = "550e8400-e29b-41d4-a716-446655440001";
        let id2 = "550e8400-e29b-41d4-a716-446655440002";
        let id3 = "550e8400-e29b-41d4-a716-446655440003";
        let result = BatchUpdateResult {
            successful: 5,
            failed: 2,
            successful_objects: vec![
                id2.to_string(),
                id3.to_string(),
            ],
            errors: vec![(
                id1.to_string(),
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::Other),
                    "test error".to_string(),
                )),
            )],
        };

        assert_eq!(result.successful, 5);
        assert_eq!(result.failed, 2);
        assert_eq!(result.successful_objects.len(), 2);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].0, id1);
    }

    #[test]
    fn test_batch_update_empty_list() {
        let _log_guard = init_test_logger();
        let client = create_test_client();
        let objects: Vec<(&MantaObject, Uuid, Option<&str>)> = vec![];

        let result = batch_update(&client, objects);
        assert!(result.is_ok());

        let batch_result = result.unwrap();
        assert_eq!(batch_result.successful, 0);
        assert_eq!(batch_result.failed, 0);
        assert_eq!(batch_result.successful_objects.len(), 0);
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
        let _log_guard = init_test_logger();
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
    fn test_should_use_mdapi_with_shards() {
        let config = MdapiConfig {
            shards: vec![
                MdapiShard {
                    host: "1.buckets-mdapi.example.com".to_string(),
                },
            ],
            connection_timeout_ms: 5000,
            max_batch_size: 100,
            operation_timeout_ms: 30000,
            max_retries: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 5000,
        };

        assert_eq!(should_use_mdapi(&config), true);
    }

    #[test]
    fn test_should_use_mdapi_empty_shards() {
        let config = MdapiConfig {
            shards: vec![],
            connection_timeout_ms: 5000,
            max_batch_size: 100,
            operation_timeout_ms: 30000,
            max_retries: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 5000,
        };

        assert_eq!(should_use_mdapi(&config), false);
    }

    #[test]
    fn test_should_use_mdapi_default_config() {
        let config = MdapiConfig::default();
        // Default config has empty shards so should_use_mdapi is false
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
            successful_objects: vec![
                "550e8400-e29b-41d4-a716-446655440001".to_string(),
                "550e8400-e29b-41d4-a716-446655440002".to_string(),
            ],
            errors: vec![],
        };

        assert_eq!(result.successful, 10);
        assert_eq!(result.failed, 0);
        assert!(result.errors.is_empty());
        assert_eq!(result.successful_objects.len(), 2);
    }

    #[test]
    fn test_batch_update_result_all_failed() {
        let result = BatchUpdateResult {
            successful: 0,
            failed: 5,
            successful_objects: vec![],
            errors: vec![
                (
                    "550e8400-e29b-41d4-a716-446655440001".to_string(),
                    Error::Internal(InternalError::new(
                        Some(InternalErrorCode::Other),
                        "error 1".to_string(),
                    )),
                ),
                (
                    "550e8400-e29b-41d4-a716-446655440002".to_string(),
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
        assert!(result.successful_objects.is_empty());
    }

    #[test]
    fn test_batch_update_result_partial_failure() {
        let id_ok1 = "550e8400-e29b-41d4-a716-446655440001".to_string();
        let id_ok2 = "550e8400-e29b-41d4-a716-446655440002".to_string();
        let id_fail = "550e8400-e29b-41d4-a716-446655440003".to_string();
        let result = BatchUpdateResult {
            successful: 8,
            failed: 2,
            successful_objects: vec![id_ok1.clone(), id_ok2.clone()],
            errors: vec![(
                id_fail.clone(),
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::MetadataUpdateFailure),
                    "update failed".to_string(),
                )),
            )],
        };

        assert_eq!(result.successful, 8);
        assert_eq!(result.failed, 2);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].0, id_fail);
        // Verify we can identify which objects succeeded
        assert_eq!(result.successful_objects.len(), 2);
        assert!(result.successful_objects.contains(&id_ok1));
    }

    #[test]
    fn test_vnode_calculation_consistency() {
        let _log_guard = init_test_logger();
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
        let _log_guard = init_test_logger();
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
        let _log_guard = init_test_logger();
        let bucket = "660e8400-e29b-41d4-a716-446655440001";
        let key = "test.txt";

        let vnode1 = calculate_vnode("owner1", bucket, key);
        let vnode2 = calculate_vnode("owner2", bucket, key);

        assert_ne!(vnode1, vnode2);
    }

    #[test]
    fn test_vnode_calculation_different_buckets() {
        let _log_guard = init_test_logger();
        let owner = "550e8400-e29b-41d4-a716-446655440000";
        let key = "test.txt";

        let vnode1 = calculate_vnode(owner, "bucket1", key);
        let vnode2 = calculate_vnode(owner, "bucket2", key);

        assert_ne!(vnode1, vnode2);
    }

    #[test]
    fn test_vnode_calculation_empty_strings() {
        let _log_guard = init_test_logger();
        // Edge case: empty strings should still produce valid vnode
        let vnode = calculate_vnode("", "", "");
        // Should not panic and should return some value
        // Vnode range is 0 to 2^32 (MD5 128bit / 2^96 hash interval)
        assert!(vnode <= u32::MAX as u64);
    }

    #[test]
    fn test_vnode_calculation_special_characters() {
        let _log_guard = init_test_logger();
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
        let _log_guard = init_test_logger();
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
        let _log_guard = init_test_logger();
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

    // =========================================================================
    // Tests for batch chunking functionality
    // =========================================================================

    #[test]
    fn test_batch_update_with_config_empty_list() {
        let _log_guard = init_test_logger();
        let client = create_test_client();
        let objects: Vec<(&MantaObject, Uuid, Option<&str>)> = vec![];

        let result = batch_update_with_config(&client, objects, Some(10));
        assert!(result.is_ok());

        let batch_result = result.unwrap();
        assert_eq!(batch_result.successful, 0);
        assert_eq!(batch_result.failed, 0);
    }

    #[test]
    fn test_batch_update_with_config_within_limit() {
        let _log_guard = init_test_logger();
        let client = create_test_client();
        let objects: Vec<(&MantaObject, Uuid, Option<&str>)> = vec![];

        // Empty list with max_batch_size of 100
        let result = batch_update_with_config(&client, objects, Some(100));
        assert!(result.is_ok());
    }

    #[test]
    fn test_default_max_batch_size_constant() {
        // Verify the default is a reasonable value
        assert_eq!(DEFAULT_MAX_BATCH_SIZE, 100);
        assert!(DEFAULT_MAX_BATCH_SIZE > 0);
        assert!(DEFAULT_MAX_BATCH_SIZE <= 1000); // Sanity check
    }

    #[test]
    fn test_batch_update_result_chunked_aggregation() {
        // Test that BatchUpdateResult can properly aggregate chunked results
        let mut total = BatchUpdateResult {
            successful: 0,
            failed: 0,
            successful_objects: Vec::new(),
            errors: Vec::new(),
        };

        // Simulate aggregating 3 chunk results
        let chunk1 = BatchUpdateResult {
            successful: 10,
            failed: 2,
            successful_objects: vec![
                "550e8400-e29b-41d4-a716-446655440001".to_string(),
                "550e8400-e29b-41d4-a716-446655440002".to_string(),
            ],
            errors: vec![(
                "550e8400-e29b-41d4-a716-446655440099".to_string(),
                Error::Internal(InternalError::new(
                    Some(InternalErrorCode::Other),
                    "test".to_string(),
                )),
            )],
        };

        let chunk2 = BatchUpdateResult {
            successful: 8,
            failed: 1,
            successful_objects: vec![
                "550e8400-e29b-41d4-a716-446655440003".to_string(),
            ],
            errors: vec![],
        };

        // Aggregate manually (simulating what batch_update_with_config does)
        total.successful += chunk1.successful + chunk2.successful;
        total.failed += chunk1.failed + chunk2.failed;
        total.successful_objects.extend(chunk1.successful_objects);
        total.successful_objects.extend(chunk2.successful_objects);
        total.errors.extend(chunk1.errors);
        total.errors.extend(chunk2.errors);

        assert_eq!(total.successful, 18);
        assert_eq!(total.failed, 3);
        assert_eq!(total.successful_objects.len(), 3);
        assert_eq!(total.errors.len(), 1);
    }

    #[test]
    fn test_batch_update_backward_compatibility() {
        let _log_guard = init_test_logger();
        let client = create_test_client();
        let objects: Vec<(&MantaObject, Uuid, Option<&str>)> = vec![];

        // Original batch_update should still work
        let result = batch_update(&client, objects);
        assert!(result.is_ok());
    }

    // =========================================================================
    // Tests for retry configuration and exponential backoff
    // =========================================================================

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_backoff_ms, 100);
        assert_eq!(config.max_backoff_ms, 5000);
    }

    #[test]
    fn test_retry_config_from_mdapi_config() {
        let mdapi_config = MdapiConfig {
            shards: vec![MdapiShard {
                host: "localhost:2030".to_string(),
            }],
            connection_timeout_ms: 5000,
            max_batch_size: 100,
            operation_timeout_ms: 30000,
            max_retries: 5,
            initial_backoff_ms: 200,
            max_backoff_ms: 10000,
        };

        let retry_config = RetryConfig::from(&mdapi_config);
        assert_eq!(retry_config.max_retries, 5);
        assert_eq!(retry_config.initial_backoff_ms, 200);
        assert_eq!(retry_config.max_backoff_ms, 10000);
    }

    #[test]
    fn test_calculate_backoff_exponential() {
        let initial = 100;
        let max = 5000;

        // First attempt: 100ms
        assert_eq!(calculate_backoff(0, initial, max), Duration::from_millis(100));
        // Second attempt: 200ms (100 * 2^1)
        assert_eq!(calculate_backoff(1, initial, max), Duration::from_millis(200));
        // Third attempt: 400ms (100 * 2^2)
        assert_eq!(calculate_backoff(2, initial, max), Duration::from_millis(400));
        // Fourth attempt: 800ms (100 * 2^3)
        assert_eq!(calculate_backoff(3, initial, max), Duration::from_millis(800));
    }

    #[test]
    fn test_calculate_backoff_capped_at_max() {
        let initial = 100;
        let max = 500;

        // Large attempt number should cap at max
        assert_eq!(calculate_backoff(10, initial, max), Duration::from_millis(500));
        assert_eq!(calculate_backoff(20, initial, max), Duration::from_millis(500));
    }

    #[test]
    fn test_calculate_backoff_overflow_protection() {
        let initial = 100;
        let max = 10000;

        // Very large attempt numbers should not overflow
        let result = calculate_backoff(50, initial, max);
        assert_eq!(result, Duration::from_millis(max));
    }

    #[test]
    fn test_with_retry_succeeds_first_try() {
        let config = RetryConfig {
            max_retries: 3,
            initial_backoff_ms: 10,
            max_backoff_ms: 100,
        };

        let mut call_count = 0;
        let result: Result<i32, Error> = with_retry(&config, || {
            call_count += 1;
            Ok(42)
        });

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count, 1);
    }

    #[test]
    fn test_with_retry_succeeds_after_failures() {
        let _log_guard = init_test_logger();
        let config = RetryConfig {
            max_retries: 3,
            initial_backoff_ms: 1, // Very short for testing
            max_backoff_ms: 10,
        };

        let mut call_count = 0;
        let result: Result<i32, Error> = with_retry(&config, || {
            call_count += 1;
            if call_count < 3 {
                Err(Error::Internal(InternalError::new(
                    Some(InternalErrorCode::Other),
                    format!("Failure {}", call_count),
                )))
            } else {
                Ok(42)
            }
        });

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count, 3);
    }

    #[test]
    fn test_with_retry_all_failures() {
        let _log_guard = init_test_logger();
        let config = RetryConfig {
            max_retries: 2,
            initial_backoff_ms: 1,
            max_backoff_ms: 10,
        };

        let mut call_count = 0;
        let result: Result<i32, Error> = with_retry(&config, || {
            call_count += 1;
            Err(Error::Internal(InternalError::new(
                Some(InternalErrorCode::Other),
                format!("Failure {}", call_count),
            )))
        });

        assert!(result.is_err());
        assert_eq!(call_count, 3); // initial + 2 retries
    }

    #[test]
    fn test_with_retry_zero_retries() {
        let config = RetryConfig {
            max_retries: 0,
            initial_backoff_ms: 1,
            max_backoff_ms: 10,
        };

        let mut call_count = 0;
        let result: Result<i32, Error> = with_retry(&config, || {
            call_count += 1;
            Err(Error::Internal(InternalError::new(
                Some(InternalErrorCode::Other),
                "Failure".to_string(),
            )))
        });

        assert!(result.is_err());
        assert_eq!(call_count, 1); // No retries, just the initial attempt
    }

    #[test]
    fn test_is_retryable_error() {
        // Client errors are retryable
        let client_error = Error::Internal(InternalError::new(
            Some(InternalErrorCode::BadMdapiClient),
            "Client error".to_string(),
        ));
        assert!(is_retryable_error(&client_error));

        // Other internal errors are retryable (may be transient)
        let other_error = Error::Internal(InternalError::new(
            Some(InternalErrorCode::Other),
            "Unknown error".to_string(),
        ));
        assert!(is_retryable_error(&other_error));

        // Metadata update failure is not retryable
        let metadata_error = Error::Internal(InternalError::new(
            Some(InternalErrorCode::MetadataUpdateFailure),
            "Update failed".to_string(),
        ));
        assert!(!is_retryable_error(&metadata_error));

        // Bucket/object not found is not retryable
        let not_found_error = Error::Internal(InternalError::new(
            Some(InternalErrorCode::MdapiBucketNotFound),
            "Bucket not found".to_string(),
        ));
        assert!(!is_retryable_error(&not_found_error));

        // Transient mdapi errors are retryable (EAGAIN, connection reset)
        use libmanta::mdapi::MdapiError;
        let io_error = Error::Mdapi(MdapiError::IoError(
            "os error 11 (EAGAIN)".to_string(),
        ));
        assert!(is_retryable_error(&io_error));

        let rpc_error = Error::Mdapi(MdapiError::RpcError(
            "connection reset".to_string(),
        ));
        assert!(is_retryable_error(&rpc_error));

        let db_error = Error::Mdapi(MdapiError::DatabaseError(
            "temporary failure".to_string(),
        ));
        assert!(is_retryable_error(&db_error));

        // Permanent mdapi errors are NOT retryable
        let not_found = Error::Mdapi(MdapiError::ObjectNotFound(
            "object gone".to_string(),
        ));
        assert!(!is_retryable_error(&not_found));

        let precondition = Error::Mdapi(MdapiError::PreconditionFailed(
            "etag mismatch".to_string(),
        ));
        assert!(!is_retryable_error(&precondition));

        let bucket_exists = Error::Mdapi(MdapiError::BucketAlreadyExists(
            "conflict".to_string(),
        ));
        assert!(!is_retryable_error(&bucket_exists));
    }
}
