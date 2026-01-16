// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A synchronous TCP connector for use with qorb connection pools.
//!
//! This connector creates `std::net::TcpStream` connections, which are
//! blocking. This is needed because the Fast RPC protocol uses synchronous
//! I/O operations. The connection is created in a blocking task to avoid
//! blocking the async runtime.

use async_trait::async_trait;
use qorb::backend::{Backend, Connector, Error};
use std::net::TcpStream;
use std::time::Duration;

/// A connector that creates synchronous TCP streams.
///
/// This is used with qorb pools to create connections that can be used
/// with the synchronous Fast RPC client.
#[derive(Clone)]
pub struct SyncTcpConnector {
    /// Connection timeout
    pub connect_timeout: Duration,
}

impl Default for SyncTcpConnector {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
        }
    }
}

impl SyncTcpConnector {
    /// Create a new connector with the specified connection timeout.
    pub fn new(connect_timeout: Duration) -> Self {
        Self { connect_timeout }
    }
}

#[async_trait]
impl Connector for SyncTcpConnector {
    type Connection = TcpStream;

    async fn connect(
        &self,
        backend: &Backend,
    ) -> Result<Self::Connection, Error> {
        let addr = backend.address;
        let timeout = self.connect_timeout;

        // Use spawn_blocking to create the sync TcpStream without blocking
        // the async runtime
        let stream = tokio::task::spawn_blocking(move || {
            TcpStream::connect_timeout(&addr, timeout)
        })
        .await
        .map_err(|e| Error::Other(e.into()))?
        .map_err(|e| Error::Other(e.into()))?;

        // Set read/write timeouts for the connection
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

        Ok(stream)
    }

    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Error> {
        // Check if the connection is still valid by peeking
        // A sync TcpStream can use peek() but we need to be careful
        // For now, just check if we can get the peer address
        conn.peer_addr()
            .map(|_| ())
            .map_err(|e| Error::Other(e.into()))
    }
}
