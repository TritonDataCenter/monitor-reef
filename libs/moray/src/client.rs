// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Moray client implementation using qorb connection pooling.
//!
//! # Resolvers
//!
//! The client supports multiple resolver types for service discovery:
//!
//! - **Fixed addresses**: Use [`MorayClient::new`] or [`MorayClient::from_str`]
//!   for testing or when connecting to a known address.
//!
//! - **Manatee/ZooKeeper**: Use [`MorayClient::with_manatee`] for production
//!   deployments with automatic primary discovery via ZooKeeper.
//!
//! - **Custom resolver**: Use [`MorayClient::with_resolver`] for any qorb
//!   resolver implementation (DNS, custom service discovery, etc.).

use qorb::policy::Policy;
use qorb::pool::Pool;
use qorb::resolver::BoxedResolver;
use qorb::resolvers::fixed::FixedResolver;

use slog::Logger;
use std::io::Error;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use super::buckets;
use super::connector::SyncTcpConnector;
use super::meta;
use super::objects;

/// A client for interacting with a Moray service.
///
/// The client maintains a connection pool to the Moray server and provides
/// async methods for interacting with buckets and objects.
pub struct MorayClient {
    pool: Pool<TcpStream>,
    #[allow(dead_code)]
    log: Logger,
}

/// Options for configuring the Moray client connection pool.
#[derive(Clone)]
pub struct ConnectionOptions {
    /// Maximum number of connections in the pool (per backend).
    pub max_connections: u32,
    /// Timeout for claiming a connection from the pool (in milliseconds).
    pub claim_timeout_ms: u64,
    /// Logger for pool operations.
    pub log: Option<Logger>,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            max_connections: 2,
            claim_timeout_ms: 5000,
            log: None,
        }
    }
}

impl MorayClient {
    /// Create a new MorayClient connected to the specified address.
    ///
    /// # Arguments
    ///
    /// * `address` - The socket address of the Moray server
    /// * `log` - A logger for client operations
    /// * `opts` - Optional connection pool configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the connection pool cannot be created.
    pub fn new(
        address: SocketAddr,
        log: Logger,
        opts: Option<ConnectionOptions>,
    ) -> Result<MorayClient, Error> {
        let _opts = opts.unwrap_or_default();

        // Create a fixed resolver with the single backend
        let resolver = Box::new(FixedResolver::new([address]));

        // Create the connector
        let connector = Arc::new(SyncTcpConnector::default());

        // Create the pool with default policy
        let policy = Policy::default();
        let pool = Pool::new("moray".to_string(), resolver, connector, policy)
            .unwrap_or_else(|e| {
                // RegistrationError contains the pool - probe registration failed but
                // the pool is still usable. Just log and continue.
                e.into_inner()
            });

        Ok(MorayClient { pool, log })
    }

    /// Create a new MorayClient from IP address components.
    ///
    /// # Arguments
    ///
    /// * `ip` - The IP address (can be any type that converts to IpAddr)
    /// * `port` - The port number
    /// * `log` - A logger for client operations
    /// * `opts` - Optional connection pool configuration
    pub fn from_parts<I: Into<IpAddr>>(
        ip: I,
        port: u16,
        log: Logger,
        opts: Option<ConnectionOptions>,
    ) -> Result<MorayClient, Error> {
        Self::new(SocketAddr::new(ip.into(), port), log, opts)
    }

    /// Create a new MorayClient from a string address.
    ///
    /// # Arguments
    ///
    /// * `s` - The address string in "ip:port" format
    /// * `log` - A logger for client operations
    /// * `opts` - Optional connection pool configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the address cannot be parsed.
    pub fn from_str(
        s: &str,
        log: Logger,
        opts: Option<ConnectionOptions>,
    ) -> Result<MorayClient, Error> {
        let addr = SocketAddr::from_str(s).map_err(|e| {
            Error::other(format!("Error parsing address '{}': {}", s, e))
        })?;
        Self::new(addr, log, opts)
    }

    /// Create a new MorayClient with a custom resolver.
    ///
    /// This is the most flexible constructor, allowing any qorb resolver
    /// to be used for service discovery. Use this for:
    ///
    /// - Production deployments with Manatee (see [`Self::with_manatee`])
    /// - DNS-based service discovery
    /// - Custom service discovery mechanisms
    ///
    /// # Arguments
    ///
    /// * `resolver` - A boxed qorb resolver for service discovery
    /// * `log` - A logger for client operations
    /// * `_opts` - Optional connection pool configuration (reserved for future use)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use qorb::resolvers::fixed::FixedResolver;
    /// use moray::MorayClient;
    ///
    /// let resolver = Box::new(FixedResolver::new(["10.0.0.1:2020".parse().unwrap()]));
    /// let client = MorayClient::with_resolver(resolver, log, None)?;
    /// ```
    pub fn with_resolver(
        resolver: BoxedResolver,
        log: Logger,
        _opts: Option<ConnectionOptions>,
    ) -> Result<MorayClient, Error> {
        let connector = Arc::new(SyncTcpConnector::default());
        let policy = Policy::default();
        let pool = Pool::new("moray".to_string(), resolver, connector, policy)
            .unwrap_or_else(|e| {
                // RegistrationError contains the pool - probe registration failed but
                // the pool is still usable. Just log and continue.
                e.into_inner()
            });

        Ok(MorayClient { pool, log })
    }

    /// Create a new MorayClient using Manatee/ZooKeeper for service discovery.
    ///
    /// This constructor connects to ZooKeeper to discover the current Manatee
    /// primary and automatically follows primary changes. Use this for
    /// production deployments.
    ///
    /// # Arguments
    ///
    /// * `zk_connect_string` - Comma-separated list of ZooKeeper addresses
    ///   (e.g., "10.0.0.1:2181,10.0.0.2:2181")
    /// * `shard_path` - The ZooKeeper path for the Manatee shard
    ///   (e.g., "/manatee/1.moray.my-region.example.com")
    /// * `log` - A logger for client operations
    /// * `opts` - Optional connection pool configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the ZooKeeper connect string is invalid.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use moray::MorayClient;
    ///
    /// let client = MorayClient::with_manatee(
    ///     "10.0.0.1:2181,10.0.0.2:2181,10.0.0.3:2181",
    ///     "/manatee/1.moray.my-region.example.com",
    ///     log,
    ///     None,
    /// )?;
    /// ```
    pub fn with_manatee(
        zk_connect_string: &str,
        shard_path: &str,
        log: Logger,
        opts: Option<ConnectionOptions>,
    ) -> Result<MorayClient, Error> {
        use qorb_manatee_resolver::ManateeResolver;

        let resolver = ManateeResolver::new(zk_connect_string, shard_path)
            .map_err(|e| {
                Error::other(format!("Failed to create ManateeResolver: {}", e))
            })?;

        Self::with_resolver(Box::new(resolver), log, opts)
    }

    /// List all buckets in the Moray service.
    ///
    /// # Arguments
    ///
    /// * `opts` - Method options for the request
    /// * `bucket_handler` - Callback invoked for each bucket found
    pub async fn list_buckets<F>(
        &self,
        opts: buckets::MethodOptions,
        mut bucket_handler: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&buckets::Bucket) -> Result<(), Error>,
    {
        let handle =
            self.pool.claim().await.map_err(|e| {
                Error::other(format!("Pool claim failed: {:?}", e))
            })?;

        let mut stream = (*handle).try_clone().map_err(|e| {
            Error::other(format!("Failed to clone stream: {}", e))
        })?;

        // Use block_in_place to run blocking I/O while allowing other tasks
        // on this runtime to make progress on other threads
        tokio::task::block_in_place(|| {
            buckets::get_list_buckets(
                &mut stream,
                "",
                opts,
                buckets::Methods::List,
                &mut bucket_handler,
            )
        })
    }

    /// Get a specific bucket by name.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the bucket to retrieve
    /// * `opts` - Method options for the request
    /// * `bucket_handler` - Callback invoked with the bucket if found
    pub async fn get_bucket<F>(
        &self,
        name: &str,
        opts: buckets::MethodOptions,
        mut bucket_handler: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&buckets::Bucket) -> Result<(), Error>,
    {
        let handle =
            self.pool.claim().await.map_err(|e| {
                Error::other(format!("Pool claim failed: {:?}", e))
            })?;

        let mut stream = (*handle).try_clone().map_err(|e| {
            Error::other(format!("Failed to clone stream: {}", e))
        })?;

        tokio::task::block_in_place(|| {
            buckets::get_list_buckets(
                &mut stream,
                name,
                opts,
                buckets::Methods::Get,
                &mut bucket_handler,
            )
        })
    }

    /// Get a specific object by key.
    ///
    /// # Arguments
    ///
    /// * `bucket` - The name of the bucket containing the object
    /// * `key` - The key of the object to retrieve
    /// * `opts` - Method options for the request
    /// * `object_handler` - Callback invoked with the object if found
    pub async fn get_object<F>(
        &self,
        bucket: &str,
        key: &str,
        opts: &objects::MethodOptions,
        mut object_handler: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&objects::MorayObject) -> Result<(), Error>,
    {
        let handle =
            self.pool.claim().await.map_err(|e| {
                Error::other(format!("Pool claim failed: {:?}", e))
            })?;

        let mut stream = (*handle).try_clone().map_err(|e| {
            Error::other(format!("Failed to clone stream: {}", e))
        })?;

        tokio::task::block_in_place(|| {
            objects::get_find_objects(
                &mut stream,
                bucket,
                key,
                opts,
                objects::Methods::Get,
                &mut object_handler,
            )
        })
    }

    /// Find objects matching a filter.
    ///
    /// # Arguments
    ///
    /// * `bucket` - The name of the bucket to search
    /// * `filter` - LDAP-style filter string
    /// * `opts` - Method options for the request
    /// * `object_handler` - Callback invoked for each matching object
    pub async fn find_objects<F>(
        &self,
        bucket: &str,
        filter: &str,
        opts: &objects::MethodOptions,
        mut object_handler: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&objects::MorayObject) -> Result<(), Error>,
    {
        let handle =
            self.pool.claim().await.map_err(|e| {
                Error::other(format!("Pool claim failed: {:?}", e))
            })?;

        let mut stream = (*handle).try_clone().map_err(|e| {
            Error::other(format!("Failed to clone stream: {}", e))
        })?;

        tokio::task::block_in_place(|| {
            objects::get_find_objects(
                &mut stream,
                bucket,
                filter,
                opts,
                objects::Methods::Find,
                &mut object_handler,
            )
        })
    }

    /// Store an object in a bucket.
    ///
    /// # Arguments
    ///
    /// * `bucket` - The name of the bucket
    /// * `key` - The key for the object
    /// * `value` - The JSON value to store
    /// * `opts` - Method options for the request
    /// * `object_handler` - Callback invoked with the etag of the stored object
    pub async fn put_object<F>(
        &self,
        bucket: &str,
        key: &str,
        value: Value,
        opts: &objects::MethodOptions,
        mut object_handler: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&str) -> Result<(), Error>,
    {
        let handle =
            self.pool.claim().await.map_err(|e| {
                Error::other(format!("Pool claim failed: {:?}", e))
            })?;

        let mut stream = (*handle).try_clone().map_err(|e| {
            Error::other(format!("Failed to clone stream: {}", e))
        })?;

        tokio::task::block_in_place(|| {
            objects::put_object(
                &mut stream,
                bucket,
                key,
                value,
                opts,
                &mut object_handler,
            )
        })
    }

    /// Create a new bucket.
    ///
    /// # Arguments
    ///
    /// * `name` - The name for the new bucket
    /// * `config` - Bucket configuration as JSON
    /// * `opts` - Method options for the request
    pub async fn create_bucket(
        &self,
        name: &str,
        config: Value,
        opts: buckets::MethodOptions,
    ) -> Result<(), Error> {
        let handle =
            self.pool.claim().await.map_err(|e| {
                Error::other(format!("Pool claim failed: {:?}", e))
            })?;

        let mut stream = (*handle).try_clone().map_err(|e| {
            Error::other(format!("Failed to clone stream: {}", e))
        })?;

        tokio::task::block_in_place(|| {
            buckets::create_bucket(&mut stream, name, config, opts)
        })
    }

    /// Execute a batch of operations atomically.
    ///
    /// # Arguments
    ///
    /// * `requests` - The batch operations to execute
    /// * `opts` - Method options for the request
    /// * `batch_handler` - Callback invoked with the batch results
    pub async fn batch<F>(
        &self,
        requests: &[objects::BatchRequest],
        opts: &objects::MethodOptions,
        mut batch_handler: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Vec<Value>) -> Result<(), Error>,
    {
        let handle =
            self.pool.claim().await.map_err(|e| {
                Error::other(format!("Pool claim failed: {:?}", e))
            })?;

        let mut stream = (*handle).try_clone().map_err(|e| {
            Error::other(format!("Failed to clone stream: {}", e))
        })?;

        tokio::task::block_in_place(|| {
            objects::batch(&mut stream, requests, opts, &mut batch_handler)
        })
    }

    /// Execute a raw SQL query.
    ///
    /// # Arguments
    ///
    /// * `stmt` - The SQL statement to execute
    /// * `vals` - Parameter values for the statement
    /// * `opts` - Query options
    /// * `query_handler` - Callback invoked with query results
    pub async fn sql<F, V>(
        &self,
        stmt: &str,
        vals: Vec<&str>,
        opts: V,
        mut query_handler: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&Value) -> Result<(), Error>,
        V: Into<Value>,
    {
        let handle =
            self.pool.claim().await.map_err(|e| {
                Error::other(format!("Pool claim failed: {:?}", e))
            })?;

        let mut stream = (*handle).try_clone().map_err(|e| {
            Error::other(format!("Failed to clone stream: {}", e))
        })?;

        tokio::task::block_in_place(|| {
            meta::sql(&mut stream, stmt, vals, opts, &mut query_handler)
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        assert_eq!(1, 1);
    }
}
