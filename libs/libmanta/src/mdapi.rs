// Copyright 2025 Edgecast Cloud LLC.

//! Client library for manta-buckets-mdapi Fast RPC service
//!
//! This module provides a Rust client for interacting with the manta-buckets-mdapi
//! service, which manages bucket and object metadata for Manta using the Fast RPC
//! protocol.
//!
//! # Example
//!
//! ```no_run
//! use libmanta::mdapi::MdapiClient;
//! use uuid::Uuid;
//!
//! let client = MdapiClient::new("mdapi.example.com:2030")?;
//! let bucket = client.get_bucket(
//!     Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")?,
//!     "mybucket",
//!     0
//! )?;
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use uuid::Uuid;

/// Errors that can occur during mdapi client operations
#[derive(Debug)]
pub enum MdapiError {
    /// Bucket already exists (conflict on create)
    BucketAlreadyExists(String),
    /// Bucket not found
    BucketNotFound(String),
    /// Object not found
    ObjectNotFound(String),
    /// Invalid pagination limit (must be 1-1024)
    InvalidLimit(u32),
    /// Precondition failed (if-match, if-modified-since, etc.)
    PreconditionFailed(String),
    /// Database/PostgreSQL error
    DatabaseError(String),
    /// Invalid MD5 content
    InvalidContentMd5(String),
    /// Fast RPC protocol error
    RpcError(String),
    /// JSON serialization/deserialization error
    SerializationError(String),
    /// Network/IO error
    IoError(String),
    /// Other unclassified errors
    Other(String),
}

impl fmt::Display for MdapiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MdapiError::BucketAlreadyExists(msg) => {
                write!(f, "Bucket already exists: {}", msg)
            }
            MdapiError::BucketNotFound(msg) => {
                write!(f, "Bucket not found: {}", msg)
            }
            MdapiError::ObjectNotFound(msg) => {
                write!(f, "Object not found: {}", msg)
            }
            MdapiError::InvalidLimit(limit) => write!(
                f,
                "Invalid pagination limit: {} (must be 1-1024)",
                limit
            ),
            MdapiError::PreconditionFailed(msg) => {
                write!(f, "Precondition failed: {}", msg)
            }
            MdapiError::DatabaseError(msg) => {
                write!(f, "Database error: {}", msg)
            }
            MdapiError::InvalidContentMd5(msg) => {
                write!(f, "Invalid content MD5: {}", msg)
            }
            MdapiError::RpcError(msg) => write!(f, "RPC error: {}", msg),
            MdapiError::SerializationError(msg) => {
                write!(f, "Serialization error: {}", msg)
            }
            MdapiError::IoError(msg) => write!(f, "IO error: {}", msg),
            MdapiError::Other(msg) => write!(f, "Error: {}", msg),
        }
    }
}

impl std::error::Error for MdapiError {}

impl From<serde_json::Error> for MdapiError {
    fn from(err: serde_json::Error) -> Self {
        MdapiError::SerializationError(err.to_string())
    }
}

impl From<std::io::Error> for MdapiError {
    fn from(err: std::io::Error) -> Self {
        MdapiError::IoError(err.to_string())
    }
}

/// Bucket metadata response
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bucket {
    /// Unique bucket identifier
    pub id: Uuid,
    /// Owner account UUID
    pub owner: Uuid,
    /// Bucket name
    pub name: String,
    /// Creation timestamp (ISO 8601)
    pub created: String,
}

/// Object metadata request payload for create operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectPayload {
    /// Owner account UUID
    pub owner: Uuid,
    /// Parent bucket UUID
    pub bucket_id: Uuid,
    /// Object name/key
    pub name: String,
    /// Object UUID
    pub id: Uuid,
    /// Virtual node (shard) identifier
    pub vnode: u64,
    /// Content size in bytes
    pub content_length: u64,
    /// Base64-encoded MD5 hash
    pub content_md5: String,
    /// MIME content type
    pub content_type: String,
    /// HTTP headers
    pub headers: HashMap<String, String>,
    /// Storage node locations
    pub sharks: Vec<StorageNodeIdentifier>,
    /// Additional properties (nullable)
    pub properties: Option<Value>,
    /// Request identifier
    pub request_id: Uuid,
    /// Conditional request parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Conditions>,
}

/// Storage node location identifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StorageNodeIdentifier {
    /// Datacenter name
    pub datacenter: String,
    /// Manta storage ID
    pub manta_storage_id: String,
}

/// Object metadata update request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectUpdate {
    /// Owner account UUID
    pub owner: Uuid,
    /// Parent bucket UUID
    pub bucket_id: Uuid,
    /// Object name/key
    pub name: String,
    /// Virtual node (shard) identifier
    pub vnode: u64,
    /// Request identifier
    pub request_id: Uuid,
    /// Updated sharks (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharks: Option<Vec<StorageNodeIdentifier>>,
    /// Updated headers (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    /// Conditional request parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Conditions>,
}

/// HTTP-like conditional request parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conditions {
    /// Match if object ETag is in this list
    #[serde(rename = "if-match", skip_serializing_if = "Option::is_none")]
    pub if_match: Option<Vec<String>>,
    /// Match if object ETag is NOT in this list
    #[serde(
        rename = "if-none-match",
        skip_serializing_if = "Option::is_none"
    )]
    pub if_none_match: Option<Vec<String>>,
    /// Match if object modified since this timestamp
    #[serde(
        rename = "if-modified-since",
        skip_serializing_if = "Option::is_none"
    )]
    pub if_modified_since: Option<String>,
    /// Match if object unmodified since this timestamp
    #[serde(
        rename = "if-unmodified-since",
        skip_serializing_if = "Option::is_none"
    )]
    pub if_unmodified_since: Option<String>,
}

/// Parameters for list operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListParams {
    /// Result limit (1-1024)
    pub limit: u32,
    /// Prefix filter (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Pagination marker (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
}

/// Deleted object entry from garbage collection batch
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeletedObject {
    /// Object UUID
    pub id: Uuid,
    /// Owner account UUID
    pub owner: Uuid,
    /// Parent bucket UUID
    pub bucket_id: Uuid,
    /// Object name/key
    pub name: String,
    /// Deletion timestamp (ISO 8601)
    pub deleted: String,
}

/// Request payload for GetBucket operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GetBucketPayload {
    owner: Uuid,
    name: String,
    vnode: u64,
    request_id: Uuid,
}

/// Request payload for CreateBucket operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateBucketPayload {
    owner: Uuid,
    name: String,
    vnode: u64,
    request_id: Uuid,
}

/// Request payload for DeleteBucket operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeleteBucketPayload {
    owner: Uuid,
    name: String,
    vnode: u64,
    request_id: Uuid,
}

/// Request payload for ListBuckets operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListBucketsPayload {
    owner: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<String>,
    limit: u32,
    request_id: Uuid,
}

/// Request payload for GetObject operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GetObjectPayload {
    owner: Uuid,
    bucket_id: Uuid,
    name: String,
    vnode: u64,
    request_id: Uuid,
}

/// Request payload for DeleteObject operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeleteObjectPayload {
    owner: Uuid,
    bucket_id: Uuid,
    name: String,
    vnode: u64,
    request_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    conditions: Option<Conditions>,
}

/// Request payload for GetGCBatch operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GetGCBatchPayload {
    shard: u32,
    limit: u32,
    request_id: Uuid,
}

/// Request payload for DeleteGCBatch operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeleteGCBatchPayload {
    shard: u32,
    batch_id: Uuid,
    request_id: Uuid,
}

/// Client for manta-buckets-mdapi Fast RPC service
///
/// This client provides methods for bucket and object metadata operations.
/// It maintains a connection to the mdapi service and handles Fast RPC
/// protocol communication.
///
/// # Example
///
/// ```no_run
/// use libmanta::mdapi::MdapiClient;
///
/// let client = MdapiClient::new("mdapi.example.com:2030")?;
/// ```
pub struct MdapiClient {
    /// Connection endpoint address
    endpoint: String,
}

impl MdapiClient {
    /// Create a new mdapi client connected to the specified endpoint
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Server address in "host:port" format
    ///
    /// # Returns
    ///
    /// A Result containing the client or an error if connection fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// use libmanta::mdapi::MdapiClient;
    ///
    /// let client = MdapiClient::new("localhost:2030")?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(endpoint: &str) -> Result<Self, MdapiError> {
        Ok(MdapiClient {
            endpoint: endpoint.to_string(),
        })
    }

    /// Get the client's endpoint address
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Generate a new request ID
    fn generate_request_id() -> Uuid {
        Uuid::new_v4()
    }

    /// Validate pagination limit (must be 1-1024)
    fn validate_limit(limit: u32) -> Result<(), MdapiError> {
        if limit < 1 || limit > 1024 {
            Err(MdapiError::InvalidLimit(limit))
        } else {
            Ok(())
        }
    }

    /// Get bucket metadata
    ///
    /// # Arguments
    ///
    /// * `owner` - Owner account UUID
    /// * `name` - Bucket name
    /// * `vnode` - Virtual node (shard) identifier
    ///
    /// # Returns
    ///
    /// Result containing Bucket metadata or error
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use libmanta::mdapi::MdapiClient;
    /// # use uuid::Uuid;
    /// # let client = MdapiClient::new("localhost:2030")?;
    /// let owner = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")?;
    /// let bucket = client.get_bucket(owner, "mybucket", 0)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get_bucket(
        &self,
        owner: Uuid,
        name: &str,
        vnode: u64,
    ) -> Result<Bucket, MdapiError> {
        let _payload = GetBucketPayload {
            owner,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
        };
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// Create a new bucket
    ///
    /// # Arguments
    ///
    /// * `owner` - Owner account UUID
    /// * `name` - Bucket name
    /// * `vnode` - Virtual node (shard) identifier
    ///
    /// # Returns
    ///
    /// Result containing created Bucket metadata or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::BucketAlreadyExists` if bucket exists
    pub fn create_bucket(
        &self,
        owner: Uuid,
        name: &str,
        vnode: u64,
    ) -> Result<Bucket, MdapiError> {
        let _payload = CreateBucketPayload {
            owner,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
        };
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// Delete a bucket
    ///
    /// # Arguments
    ///
    /// * `owner` - Owner account UUID
    /// * `name` - Bucket name
    /// * `vnode` - Virtual node (shard) identifier
    ///
    /// # Returns
    ///
    /// Result indicating success or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::BucketNotFound` if bucket doesn't exist
    pub fn delete_bucket(
        &self,
        owner: Uuid,
        name: &str,
        vnode: u64,
    ) -> Result<(), MdapiError> {
        let _payload = DeleteBucketPayload {
            owner,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
        };
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// List buckets for an owner
    ///
    /// # Arguments
    ///
    /// * `owner` - Owner account UUID
    /// * `prefix` - Optional name prefix filter
    /// * `limit` - Maximum results to return (1-1024)
    ///
    /// # Returns
    ///
    /// Result containing vector of Buckets or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::InvalidLimit` if limit is out of range
    pub fn list_buckets(
        &self,
        owner: Uuid,
        prefix: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Bucket>, MdapiError> {
        Self::validate_limit(limit)?;

        let _payload = ListBucketsPayload {
            owner,
            prefix: prefix.map(String::from),
            limit,
            request_id: Self::generate_request_id(),
        };
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// Get object metadata
    ///
    /// # Arguments
    ///
    /// * `owner` - Owner account UUID
    /// * `bucket_id` - Parent bucket UUID
    /// * `name` - Object name/key
    /// * `vnode` - Virtual node (shard) identifier
    ///
    /// # Returns
    ///
    /// Result containing object value or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::ObjectNotFound` if object doesn't exist
    pub fn get_object(
        &self,
        owner: Uuid,
        bucket_id: Uuid,
        name: &str,
        vnode: u64,
    ) -> Result<Value, MdapiError> {
        let _payload = GetObjectPayload {
            owner,
            bucket_id,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
        };
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// Create a new object
    ///
    /// # Arguments
    ///
    /// * `payload` - Object creation payload with metadata
    ///
    /// # Returns
    ///
    /// Result containing created object value or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::PreconditionFailed` if conditions not met
    /// Returns `MdapiError::InvalidContentMd5` if MD5 is malformed
    pub fn create_object(
        &self,
        payload: ObjectPayload,
    ) -> Result<Value, MdapiError> {
        let _payload = payload;
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// Update an existing object
    ///
    /// # Arguments
    ///
    /// * `update` - Object update payload
    ///
    /// # Returns
    ///
    /// Result containing updated object value or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::ObjectNotFound` if object doesn't exist
    /// Returns `MdapiError::PreconditionFailed` if conditions not met
    pub fn update_object(
        &self,
        update: ObjectUpdate,
    ) -> Result<Value, MdapiError> {
        let _payload = update;
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// Delete an object
    ///
    /// # Arguments
    ///
    /// * `owner` - Owner account UUID
    /// * `bucket_id` - Parent bucket UUID
    /// * `name` - Object name/key
    /// * `vnode` - Virtual node (shard) identifier
    /// * `conditions` - Optional conditional delete parameters
    ///
    /// # Returns
    ///
    /// Result indicating success or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::ObjectNotFound` if object doesn't exist
    /// Returns `MdapiError::PreconditionFailed` if conditions not met
    pub fn delete_object(
        &self,
        owner: Uuid,
        bucket_id: Uuid,
        name: &str,
        vnode: u64,
        conditions: Option<Conditions>,
    ) -> Result<(), MdapiError> {
        let _payload = DeleteObjectPayload {
            owner,
            bucket_id,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
            conditions,
        };
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// List objects in a bucket
    ///
    /// # Arguments
    ///
    /// * `owner` - Owner account UUID
    /// * `bucket_id` - Parent bucket UUID
    /// * `params` - List parameters (limit, prefix, marker)
    ///
    /// # Returns
    ///
    /// Result containing vector of object values or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::InvalidLimit` if limit is out of range
    /// Returns `MdapiError::BucketNotFound` if bucket doesn't exist
    pub fn list_objects(
        &self,
        owner: Uuid,
        bucket_id: Uuid,
        params: ListParams,
    ) -> Result<Vec<Value>, MdapiError> {
        Self::validate_limit(params.limit)?;
        let _owner = owner;
        let _bucket_id = bucket_id;
        let _params = params;
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// Get a batch of deleted objects for garbage collection
    ///
    /// # Arguments
    ///
    /// * `shard` - Shard/vnode identifier
    /// * `limit` - Maximum number of objects to return (1-1024)
    ///
    /// # Returns
    ///
    /// Result containing vector of DeletedObject entries or error
    ///
    /// # Errors
    ///
    /// Returns `MdapiError::InvalidLimit` if limit is out of range
    pub fn get_gc_batch(
        &self,
        shard: u32,
        limit: u32,
    ) -> Result<Vec<DeletedObject>, MdapiError> {
        Self::validate_limit(limit)?;

        let _payload = GetGCBatchPayload {
            shard,
            limit,
            request_id: Self::generate_request_id(),
        };
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }

    /// Mark a garbage collection batch as processed
    ///
    /// # Arguments
    ///
    /// * `shard` - Shard/vnode identifier
    /// * `batch_id` - Batch identifier to delete
    ///
    /// # Returns
    ///
    /// Result indicating success or error
    pub fn delete_gc_batch(
        &self,
        shard: u32,
        batch_id: Uuid,
    ) -> Result<(), MdapiError> {
        let _payload = DeleteGCBatchPayload {
            shard,
            batch_id,
            request_id: Self::generate_request_id(),
        };
        // TODO: Implement Fast RPC call
        Err(MdapiError::Other("Not implemented".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_serialization() {
        let bucket = Bucket {
            id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            owner: Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap(),
            name: "test-bucket".to_string(),
            created: "2025-01-01T00:00:00.000Z".to_string(),
        };

        let json = serde_json::to_string(&bucket).unwrap();
        let deserialized: Bucket = serde_json::from_str(&json).unwrap();

        assert_eq!(bucket, deserialized);
    }

    #[test]
    fn test_storage_node_identifier_serialization() {
        let shark = StorageNodeIdentifier {
            datacenter: "us-east-1".to_string(),
            manta_storage_id: "1.stor.example.com".to_string(),
        };

        let json = serde_json::to_string(&shark).unwrap();
        let deserialized: StorageNodeIdentifier =
            serde_json::from_str(&json).unwrap();

        assert_eq!(shark, deserialized);
    }

    #[test]
    fn test_conditions_serialization() {
        let conditions = Conditions {
            if_match: Some(vec!["etag1".to_string(), "etag2".to_string()]),
            if_none_match: None,
            if_modified_since: Some("2025-01-01T00:00:00.000Z".to_string()),
            if_unmodified_since: None,
        };

        let json = serde_json::to_string(&conditions).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();

        // Verify field naming (hyphenated)
        assert!(parsed.get("if-match").is_some());
        assert!(parsed.get("if-modified-since").is_some());
        assert!(parsed.get("if-none-match").is_none());
    }

    #[test]
    fn test_list_params_limits() {
        let params = ListParams {
            limit: 100,
            prefix: Some("test/".to_string()),
            marker: None,
        };

        let json = serde_json::to_string(&params).unwrap();
        let deserialized: ListParams = serde_json::from_str(&json).unwrap();

        assert_eq!(params.limit, deserialized.limit);
        assert_eq!(params.prefix, deserialized.prefix);
    }

    #[test]
    fn test_deleted_object_serialization() {
        let deleted = DeletedObject {
            id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            owner: Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap(),
            bucket_id: Uuid::parse_str("770e8400-e29b-41d4-a716-446655440002")
                .unwrap(),
            name: "deleted-object.txt".to_string(),
            deleted: "2025-01-01T00:00:00.000Z".to_string(),
        };

        let json = serde_json::to_string(&deleted).unwrap();
        let deserialized: DeletedObject = serde_json::from_str(&json).unwrap();

        assert_eq!(deleted, deserialized);
    }

    #[test]
    fn test_mdapi_client_creation() {
        let client = MdapiClient::new("localhost:2030");
        assert!(client.is_ok());

        let client = client.unwrap();
        assert_eq!(client.endpoint(), "localhost:2030");
    }

    #[test]
    fn test_validate_limit_success() {
        assert!(MdapiClient::validate_limit(1).is_ok());
        assert!(MdapiClient::validate_limit(100).is_ok());
        assert!(MdapiClient::validate_limit(1024).is_ok());
    }

    #[test]
    fn test_validate_limit_failure() {
        let result = MdapiClient::validate_limit(0);
        assert!(result.is_err());
        assert!(matches!(result, Err(MdapiError::InvalidLimit(0))));

        let result = MdapiClient::validate_limit(1025);
        assert!(result.is_err());
        assert!(matches!(result, Err(MdapiError::InvalidLimit(1025))));
    }

    #[test]
    fn test_generate_request_id() {
        let id1 = MdapiClient::generate_request_id();
        let id2 = MdapiClient::generate_request_id();

        // UUIDs should be unique
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_error_display() {
        let err = MdapiError::BucketNotFound("test".to_string());
        assert_eq!(err.to_string(), "Bucket not found: test");

        let err = MdapiError::InvalidLimit(2000);
        assert_eq!(
            err.to_string(),
            "Invalid pagination limit: 2000 (must be 1-1024)"
        );
    }

    #[test]
    fn test_object_payload_construction() {
        let owner = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")
            .unwrap();
        let bucket_id = Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001")
            .unwrap();
        let object_id = Uuid::parse_str("770e8400-e29b-41d4-a716-446655440002")
            .unwrap();

        let sharks = vec![StorageNodeIdentifier {
            datacenter: "us-east-1".to_string(),
            manta_storage_id: "1.stor.example.com".to_string(),
        }];

        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/plain".to_string());

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "test.txt".to_string(),
            id: object_id,
            vnode: 42,
            content_length: 1024,
            content_md5: "rL0Y20zC+Fzt72VPzMSk2A==".to_string(),
            content_type: "text/plain".to_string(),
            headers,
            sharks,
            properties: None,
            request_id: Uuid::new_v4(),
            conditions: None,
        };

        // Verify serialization works
        let json = serde_json::to_value(&payload);
        assert!(json.is_ok());
    }
}
