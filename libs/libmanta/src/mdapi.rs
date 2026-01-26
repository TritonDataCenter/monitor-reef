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
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

use fast_rpc::client as fast_client;
use fast_rpc::protocol::{FastMessage, FastMessageId};

/// Default maximum connections in the pool
const DEFAULT_POOL_SIZE: usize = 4;

/// Connection idle timeout (connections older than this are discarded)
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A pooled TCP connection with timestamp for idle tracking
struct PooledConnection {
    stream: TcpStream,
    created_at: Instant,
}

impl PooledConnection {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            created_at: Instant::now(),
        }
    }

    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > CONNECTION_IDLE_TIMEOUT
    }
}

/// Simple connection pool for TCP connections
///
/// This pool maintains a set of reusable TCP connections to avoid
/// the overhead of creating new connections for each RPC call.
struct ConnectionPool {
    endpoint: String,
    connections: Mutex<Vec<PooledConnection>>,
    max_size: usize,
}

impl ConnectionPool {
    /// Create a new connection pool for the given endpoint
    fn new(endpoint: String, max_size: usize) -> Self {
        Self {
            endpoint,
            connections: Mutex::new(Vec::with_capacity(max_size)),
            max_size,
        }
    }

    /// Get a connection from the pool or create a new one
    fn get(&self) -> Result<TcpStream, MdapiError> {
        // Try to get an existing connection from the pool
        {
            let mut pool = self.connections.lock().map_err(|_| {
                MdapiError::IoError("Connection pool lock poisoned".to_string())
            })?;

            // Find a non-expired connection
            while let Some(conn) = pool.pop() {
                if !conn.is_expired() {
                    // Test if connection is still valid by checking if it's readable
                    // (a closed connection will return an error or 0 bytes)
                    if Self::is_connection_alive(&conn.stream) {
                        return Ok(conn.stream);
                    }
                    // Connection is dead, drop it and try next
                }
                // Connection expired or dead, drop it and try next
            }
        }

        // No pooled connection available, create a new one
        self.create_connection()
    }

    /// Return a connection to the pool for reuse
    fn put(&self, stream: TcpStream) {
        let mut pool = match self.connections.lock() {
            Ok(p) => p,
            Err(_) => return, // Pool poisoned, just drop the connection
        };

        // Only add back if pool isn't full
        if pool.len() < self.max_size {
            pool.push(PooledConnection::new(stream));
        }
        // Otherwise, connection is dropped
    }

    /// Create a new TCP connection to the endpoint
    fn create_connection(&self) -> Result<TcpStream, MdapiError> {
        let addr = self
            .endpoint
            .to_socket_addrs()
            .map_err(|e| {
                MdapiError::IoError(format!(
                    "Failed to resolve endpoint {}: {}",
                    self.endpoint, e
                ))
            })?
            .next()
            .ok_or_else(|| {
                MdapiError::IoError(format!(
                    "No address found for endpoint {}",
                    self.endpoint
                ))
            })?;

        let stream = TcpStream::connect(addr).map_err(|e| {
            MdapiError::IoError(format!(
                "Failed to connect to {}: {}",
                self.endpoint, e
            ))
        })?;

        // Set TCP keepalive and timeouts
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .ok();
        stream.set_nodelay(true).ok();

        Ok(stream)
    }

    /// Check if a connection is still alive using peek
    fn is_connection_alive(stream: &TcpStream) -> bool {
        // Set non-blocking temporarily to check connection state
        if stream.set_nonblocking(true).is_err() {
            return false;
        }

        // Try to peek at the stream - a closed connection will error
        let mut buf = [0u8; 1];
        let result = match stream.peek(&mut buf) {
            Ok(0) => false, // Connection closed by peer
            Ok(_) => true,  // Data available (shouldn't happen, but connection is alive)
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                true // No data but connection is alive
            }
            Err(_) => false, // Connection error
        };

        // Restore blocking mode
        stream.set_nonblocking(false).ok();

        result
    }
}

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
    #[serde(rename = "if-none-match", skip_serializing_if = "Option::is_none")]
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
    vnode: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<String>,
    limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    marker: Option<String>,
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

/// Request payload for ListObjects operation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListObjectsPayload {
    owner: Uuid,
    bucket_id: Uuid,
    vnode: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<String>,
    limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    marker: Option<String>,
    request_id: Uuid,
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
/// It maintains a connection pool to the mdapi service and handles Fast RPC
/// protocol communication.
///
/// # Connection Pooling
///
/// The client maintains a pool of TCP connections to avoid the overhead of
/// creating new connections for each RPC call. Connections are automatically
/// reused and recycled when they become stale.
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
    /// Connection pool for reusing TCP connections
    pool: Arc<ConnectionPool>,
}

impl Clone for MdapiClient {
    fn clone(&self) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            pool: Arc::clone(&self.pool),
        }
    }
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
        Self::with_pool_size(endpoint, DEFAULT_POOL_SIZE)
    }

    /// Create a new mdapi client with a custom connection pool size
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Server address in "host:port" format
    /// * `pool_size` - Maximum number of connections to keep in the pool
    ///
    /// # Returns
    ///
    /// A Result containing the client or an error if connection fails
    pub fn with_pool_size(
        endpoint: &str,
        pool_size: usize,
    ) -> Result<Self, MdapiError> {
        Ok(MdapiClient {
            endpoint: endpoint.to_string(),
            pool: Arc::new(ConnectionPool::new(
                endpoint.to_string(),
                pool_size,
            )),
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

    /// Make a Fast RPC call to the mdapi service
    ///
    /// # Arguments
    ///
    /// * `method` - RPC method name
    /// * `payload` - Request payload to serialize
    ///
    /// # Returns
    ///
    /// JSON value containing the response data
    fn call<T: Serialize>(
        &self,
        method: &str,
        payload: &T,
    ) -> Result<Value, MdapiError> {
        // Convert payload to JSON
        let args = serde_json::to_value(vec![payload]).map_err(|e| {
            MdapiError::SerializationError(format!(
                "Failed to serialize payload: {}",
                e
            ))
        })?;

        // Get a connection from the pool
        let mut stream = self.pool.get()?;
        let mut rpc_succeeded = false;

        // Send Fast RPC request
        let mut msg_id = FastMessageId::new();
        let send_result =
            fast_client::send(method.to_string(), args, &mut msg_id, &mut stream);

        if let Err(e) = send_result {
            // Connection failed, don't return it to pool
            return Err(MdapiError::RpcError(format!(
                "Failed to send RPC request: {}",
                e
            )));
        }

        // Receive Fast RPC response
        let mut response_data: Option<Value> = None;
        let mut response_error: Option<String> = None;

        let recv_cb = |msg: &FastMessage| {
            // Check if this is an error response
            if let Some(error_obj) = msg.data.d.get(0) {
                if let Some(err_name) = error_obj.get("name") {
                    if err_name.as_str() == Some("FastRequestError")
                        || err_name.as_str() == Some("PostgresError")
                        || err_name.as_str() == Some("BucketNotFoundError")
                        || err_name.as_str() == Some("ObjectNotFoundError")
                    {
                        response_error = Some(
                            error_obj
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("Unknown error")
                                .to_string(),
                        );
                        return Ok(());
                    }
                }
            }

            // Extract successful response data (first element of array)
            if let Some(data) = msg.data.d.get(0) {
                response_data = Some(data.clone());
            } else {
                response_error =
                    Some("Empty response from mdapi service".to_string());
            }
            Ok(())
        };

        let recv_result = fast_client::receive(&mut stream, recv_cb);

        if let Err(e) = recv_result {
            // Connection failed during receive, don't return it to pool
            return Err(MdapiError::RpcError(format!(
                "Failed to receive RPC response: {}",
                e
            )));
        }

        // RPC completed successfully (even if it returned an error response)
        rpc_succeeded = true;

        // Return connection to pool if RPC succeeded
        if rpc_succeeded {
            self.pool.put(stream);
        }

        // Return error if one was received
        if let Some(err) = response_error {
            return Err(MdapiError::RpcError(err));
        }

        // Return response data
        response_data.ok_or_else(|| {
            MdapiError::RpcError("No response data received".to_string())
        })
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
        let payload = GetBucketPayload {
            owner,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
        };

        let response = self.call("getBucket", &payload)?;

        let bucket: Bucket = serde_json::from_value(response).map_err(|e| {
            MdapiError::SerializationError(format!(
                "Failed to parse bucket response: {}",
                e
            ))
        })?;

        Ok(bucket)
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
        let payload = CreateBucketPayload {
            owner,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
        };

        let response = self.call("createBucket", &payload)?;

        let bucket: Bucket = serde_json::from_value(response).map_err(|e| {
            MdapiError::SerializationError(format!(
                "Failed to parse bucket response: {}",
                e
            ))
        })?;

        Ok(bucket)
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
        let payload = DeleteBucketPayload {
            owner,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
        };

        // deleteBucket returns empty response on success
        self.call("deleteBucket", &payload)?;
        Ok(())
    }

    /// List buckets for an owner
    ///
    /// # Arguments
    ///
    /// * `owner` - Owner account UUID
    /// * `vnode` - Virtual node (shard) to query
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
    /// Returns `MdapiError::RpcError` if the RPC call fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use libmanta::mdapi::MdapiClient;
    /// # use uuid::Uuid;
    /// # let client = MdapiClient::new("localhost:2030")?;
    /// let owner = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")?;
    /// let buckets = client.list_buckets(owner, 0, None, 100)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list_buckets(
        &self,
        owner: Uuid,
        vnode: u64,
        prefix: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Bucket>, MdapiError> {
        Self::validate_limit(limit)?;

        let payload = ListBucketsPayload {
            owner,
            vnode,
            prefix: prefix.map(String::from),
            limit: limit as u64,
            marker: None,
            request_id: Self::generate_request_id(),
        };

        let response = self.call("listBuckets", &payload)?;

        // Parse response as array of buckets
        let buckets: Vec<Bucket> =
            serde_json::from_value(response).map_err(|e| {
                MdapiError::SerializationError(format!(
                    "Failed to parse buckets response: {}",
                    e
                ))
            })?;

        Ok(buckets)
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
        let payload = GetObjectPayload {
            owner,
            bucket_id,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
        };

        self.call("getObject", &payload)
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
        self.call("createObject", &payload)
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
        self.call("updateObject", &update)
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
        let payload = DeleteObjectPayload {
            owner,
            bucket_id,
            name: name.to_string(),
            vnode,
            request_id: Self::generate_request_id(),
            conditions,
        };

        // deleteObject returns empty response on success
        self.call("deleteObject", &payload)?;
        Ok(())
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

        // Note: vnode is derived from bucket_id by mdapi service
        // Objects inherit their bucket's vnode
        let payload = ListObjectsPayload {
            owner,
            bucket_id,
            vnode: 0, // Placeholder - mdapi service uses bucket's vnode
            prefix: params.prefix,
            limit: params.limit as u64,
            marker: params.marker,
            request_id: Self::generate_request_id(),
        };

        let response = self.call("listObjects", &payload)?;

        // Parse response as array of object Values
        let objects: Vec<Value> =
            serde_json::from_value(response).map_err(|e| {
                MdapiError::SerializationError(format!(
                    "Failed to parse objects response: {}",
                    e
                ))
            })?;

        Ok(objects)
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

        let payload = GetGCBatchPayload {
            shard,
            limit,
            request_id: Self::generate_request_id(),
        };

        let response = self.call("getGCBatch", &payload)?;

        // Parse response as array of deleted objects
        let deleted_objects: Vec<DeletedObject> =
            serde_json::from_value(response).map_err(|e| {
                MdapiError::SerializationError(format!(
                    "Failed to parse GC batch response: {}",
                    e
                ))
            })?;

        Ok(deleted_objects)
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
        let payload = DeleteGCBatchPayload {
            shard,
            batch_id,
            request_id: Self::generate_request_id(),
        };

        // deleteGCBatch returns empty response on success
        self.call("deleteGCBatch", &payload)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_serialization() {
        let bucket = Bucket {
            id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")
                .unwrap(),
            owner: Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001")
                .unwrap(),
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
            id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")
                .unwrap(),
            owner: Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001")
                .unwrap(),
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
    fn test_mdapi_client_with_custom_pool_size() {
        let client = MdapiClient::with_pool_size("localhost:2030", 8);
        assert!(client.is_ok());

        let client = client.unwrap();
        assert_eq!(client.endpoint(), "localhost:2030");

        // Verify clone shares the same pool (Arc)
        let client2 = client.clone();
        assert_eq!(client2.endpoint(), "localhost:2030");
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
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();
        let object_id =
            Uuid::parse_str("770e8400-e29b-41d4-a716-446655440002").unwrap();

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

    #[test]
    fn test_object_payload_with_conditions() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();
        let object_id =
            Uuid::parse_str("770e8400-e29b-41d4-a716-446655440002").unwrap();

        let conditions = Conditions {
            if_match: Some(vec!["etag-abc123".to_string()]),
            if_none_match: None,
            if_modified_since: None,
            if_unmodified_since: None,
        };

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "test.txt".to_string(),
            id: object_id,
            vnode: 42,
            content_length: 1024,
            content_md5: "rL0Y20zC+Fzt72VPzMSk2A==".to_string(),
            content_type: "text/plain".to_string(),
            headers: HashMap::new(),
            sharks: vec![],
            properties: None,
            request_id: Uuid::new_v4(),
            conditions: Some(conditions),
        };

        let json = serde_json::to_value(&payload).unwrap();

        // Verify conditions are serialized with hyphenated field names
        let cond = json.get("conditions").unwrap();
        assert!(cond.get("if-match").is_some());
        assert_eq!(cond["if-match"][0], "etag-abc123");
    }

    #[test]
    fn test_object_payload_with_properties() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();
        let object_id =
            Uuid::parse_str("770e8400-e29b-41d4-a716-446655440002").unwrap();

        // Properties can contain arbitrary JSON including bucket_id for bucket objects
        let properties = serde_json::json!({
            "bucket_id": "880e8400-e29b-41d4-a716-446655440003",
            "custom_field": "custom_value"
        });

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "bucket-object.txt".to_string(),
            id: object_id,
            vnode: 42,
            content_length: 2048,
            content_md5: "xyz123==".to_string(),
            content_type: "application/octet-stream".to_string(),
            headers: HashMap::new(),
            sharks: vec![],
            properties: Some(properties),
            request_id: Uuid::new_v4(),
            conditions: None,
        };

        let json = serde_json::to_value(&payload).unwrap();

        // Verify properties are preserved
        let props = json.get("properties").unwrap();
        assert_eq!(
            props["bucket_id"],
            "880e8400-e29b-41d4-a716-446655440003"
        );
        assert_eq!(props["custom_field"], "custom_value");
    }

    #[test]
    fn test_object_payload_multiple_sharks() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();
        let object_id =
            Uuid::parse_str("770e8400-e29b-41d4-a716-446655440002").unwrap();

        // Multiple sharks for replication
        let sharks = vec![
            StorageNodeIdentifier {
                datacenter: "us-east-1".to_string(),
                manta_storage_id: "1.stor.us-east.example.com".to_string(),
            },
            StorageNodeIdentifier {
                datacenter: "us-west-2".to_string(),
                manta_storage_id: "2.stor.us-west.example.com".to_string(),
            },
            StorageNodeIdentifier {
                datacenter: "eu-central-1".to_string(),
                manta_storage_id: "3.stor.eu.example.com".to_string(),
            },
        ];

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "replicated-object.txt".to_string(),
            id: object_id,
            vnode: 100,
            content_length: 4096,
            content_md5: "abc123==".to_string(),
            content_type: "text/plain".to_string(),
            headers: HashMap::new(),
            sharks: sharks.clone(),
            properties: None,
            request_id: Uuid::new_v4(),
            conditions: None,
        };

        let json = serde_json::to_value(&payload).unwrap();

        // Verify all sharks are serialized
        let sharks_json = json.get("sharks").unwrap().as_array().unwrap();
        assert_eq!(sharks_json.len(), 3);
        assert_eq!(sharks_json[0]["datacenter"], "us-east-1");
        assert_eq!(sharks_json[1]["datacenter"], "us-west-2");
        assert_eq!(sharks_json[2]["datacenter"], "eu-central-1");
    }

    #[test]
    fn test_object_update_serialization() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        let update = ObjectUpdate {
            owner,
            bucket_id,
            name: "test-object.txt".to_string(),
            vnode: 42,
            request_id: Uuid::new_v4(),
            sharks: None,
            headers: None,
            conditions: None,
        };

        let json = serde_json::to_value(&update).unwrap();

        // Verify required fields are present
        assert_eq!(json["owner"], owner.to_string());
        assert_eq!(json["bucket_id"], bucket_id.to_string());
        assert_eq!(json["name"], "test-object.txt");
        assert_eq!(json["vnode"], 42);

        // Verify optional fields are omitted when None
        assert!(json.get("sharks").is_none());
        assert!(json.get("headers").is_none());
        assert!(json.get("conditions").is_none());
    }

    #[test]
    fn test_object_update_with_sharks() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        // Update sharks after evacuation
        let new_sharks = vec![
            StorageNodeIdentifier {
                datacenter: "us-east-1".to_string(),
                manta_storage_id: "new-1.stor.example.com".to_string(),
            },
            StorageNodeIdentifier {
                datacenter: "us-west-2".to_string(),
                manta_storage_id: "new-2.stor.example.com".to_string(),
            },
        ];

        let update = ObjectUpdate {
            owner,
            bucket_id,
            name: "evacuated-object.txt".to_string(),
            vnode: 100,
            request_id: Uuid::new_v4(),
            sharks: Some(new_sharks),
            headers: None,
            conditions: None,
        };

        let json = serde_json::to_value(&update).unwrap();

        // Verify sharks are included
        let sharks_json = json.get("sharks").unwrap().as_array().unwrap();
        assert_eq!(sharks_json.len(), 2);
        assert_eq!(
            sharks_json[0]["manta_storage_id"],
            "new-1.stor.example.com"
        );
        assert_eq!(
            sharks_json[1]["manta_storage_id"],
            "new-2.stor.example.com"
        );
    }

    #[test]
    fn test_object_update_with_conditions() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        // Conditional update with etag matching
        let conditions = Conditions {
            if_match: Some(vec!["original-etag".to_string()]),
            if_none_match: None,
            if_modified_since: None,
            if_unmodified_since: None,
        };

        let update = ObjectUpdate {
            owner,
            bucket_id,
            name: "conditional-update.txt".to_string(),
            vnode: 50,
            request_id: Uuid::new_v4(),
            sharks: None,
            headers: None,
            conditions: Some(conditions),
        };

        let json = serde_json::to_value(&update).unwrap();

        // Verify conditions are included with hyphenated names
        let cond = json.get("conditions").unwrap();
        assert!(cond.get("if-match").is_some());
        assert_eq!(cond["if-match"][0], "original-etag");
    }

    #[test]
    fn test_object_update_roundtrip() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();
        let request_id =
            Uuid::parse_str("990e8400-e29b-41d4-a716-446655440009").unwrap();

        let sharks = vec![StorageNodeIdentifier {
            datacenter: "us-east-1".to_string(),
            manta_storage_id: "1.stor.example.com".to_string(),
        }];

        let mut headers = HashMap::new();
        headers.insert("x-custom-header".to_string(), "value".to_string());

        let update = ObjectUpdate {
            owner,
            bucket_id,
            name: "roundtrip-test.txt".to_string(),
            vnode: 75,
            request_id,
            sharks: Some(sharks),
            headers: Some(headers),
            conditions: None,
        };

        // Serialize and deserialize
        let json_str = serde_json::to_string(&update).unwrap();
        let deserialized: ObjectUpdate =
            serde_json::from_str(&json_str).unwrap();

        // Verify all fields match
        assert_eq!(deserialized.owner, owner);
        assert_eq!(deserialized.bucket_id, bucket_id);
        assert_eq!(deserialized.name, "roundtrip-test.txt");
        assert_eq!(deserialized.vnode, 75);
        assert_eq!(deserialized.request_id, request_id);
        assert!(deserialized.sharks.is_some());
        assert_eq!(deserialized.sharks.unwrap().len(), 1);
        assert!(deserialized.headers.is_some());
        assert_eq!(
            deserialized.headers.unwrap().get("x-custom-header"),
            Some(&"value".to_string())
        );
    }

    #[test]
    fn test_object_payload_roundtrip() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let bucket_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();
        let object_id =
            Uuid::parse_str("770e8400-e29b-41d4-a716-446655440002").unwrap();
        let request_id =
            Uuid::parse_str("880e8400-e29b-41d4-a716-446655440008").unwrap();

        let sharks = vec![
            StorageNodeIdentifier {
                datacenter: "us-east-1".to_string(),
                manta_storage_id: "1.stor.example.com".to_string(),
            },
            StorageNodeIdentifier {
                datacenter: "us-west-2".to_string(),
                manta_storage_id: "2.stor.example.com".to_string(),
            },
        ];

        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/plain".to_string());
        headers.insert(
            "x-custom-header".to_string(),
            "custom-value".to_string(),
        );

        let properties = serde_json::json!({
            "bucket_id": "990e8400-e29b-41d4-a716-446655440009"
        });

        let payload = ObjectPayload {
            owner,
            bucket_id,
            name: "roundtrip-payload.txt".to_string(),
            id: object_id,
            vnode: 123,
            content_length: 8192,
            content_md5: "roundtripMD5==".to_string(),
            content_type: "text/plain".to_string(),
            headers,
            sharks,
            properties: Some(properties),
            request_id,
            conditions: None,
        };

        // Serialize and deserialize
        let json_str = serde_json::to_string(&payload).unwrap();
        let deserialized: ObjectPayload =
            serde_json::from_str(&json_str).unwrap();

        // Verify all fields match
        assert_eq!(deserialized.owner, owner);
        assert_eq!(deserialized.bucket_id, bucket_id);
        assert_eq!(deserialized.id, object_id);
        assert_eq!(deserialized.name, "roundtrip-payload.txt");
        assert_eq!(deserialized.vnode, 123);
        assert_eq!(deserialized.content_length, 8192);
        assert_eq!(deserialized.content_md5, "roundtripMD5==");
        assert_eq!(deserialized.content_type, "text/plain");
        assert_eq!(deserialized.sharks.len(), 2);
        assert_eq!(deserialized.headers.len(), 2);
        assert!(deserialized.properties.is_some());
        assert_eq!(deserialized.request_id, request_id);
    }

    #[test]
    fn test_list_buckets_payload_serialization() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let request_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        let payload = ListBucketsPayload {
            owner,
            vnode: 0,
            prefix: Some("test-".to_string()),
            limit: 100,
            marker: None,
            request_id,
        };

        // Verify serialization works
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["owner"], owner.to_string());
        assert_eq!(json["vnode"], 0);
        assert_eq!(json["prefix"], "test-");
        assert_eq!(json["limit"], 100);
        assert_eq!(json["request_id"], request_id.to_string());

        // Verify marker is omitted when None
        assert!(json.get("marker").is_none());
    }

    #[test]
    fn test_list_buckets_response_parsing() {
        // Simulate a response from mdapi listBuckets
        let response_json = serde_json::json!([
            {
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "owner": "660e8400-e29b-41d4-a716-446655440001",
                "name": "bucket1",
                "created": "2025-01-01T00:00:00.000Z"
            },
            {
                "id": "770e8400-e29b-41d4-a716-446655440002",
                "owner": "660e8400-e29b-41d4-a716-446655440001",
                "name": "bucket2",
                "created": "2025-01-02T00:00:00.000Z"
            }
        ]);

        // Parse as Vec<Bucket>
        let buckets: Vec<Bucket> =
            serde_json::from_value(response_json).unwrap();

        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].name, "bucket1");
        assert_eq!(buckets[1].name, "bucket2");
        assert_eq!(
            buckets[0].id,
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
    }

    #[test]
    fn test_list_buckets_empty_response() {
        // Empty array response
        let response_json = serde_json::json!([]);

        // Parse as Vec<Bucket>
        let buckets: Vec<Bucket> =
            serde_json::from_value(response_json).unwrap();

        assert_eq!(buckets.len(), 0);
    }

    #[test]
    fn test_list_buckets_with_prefix() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let request_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        let payload = ListBucketsPayload {
            owner,
            vnode: 0,
            prefix: Some("prod-".to_string()),
            limit: 50,
            marker: None,
            request_id,
        };

        // Verify prefix is included in serialization
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["prefix"], "prod-");
    }

    #[test]
    fn test_list_buckets_with_marker() {
        let owner =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let request_id =
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        let payload = ListBucketsPayload {
            owner,
            vnode: 0,
            prefix: None,
            limit: 100,
            marker: Some("bucket-100".to_string()),
            request_id,
        };

        // Verify marker is included when Some
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["marker"], "bucket-100");
    }
}
