// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Transport layer for the metadata protocol.
//!
//! Supports Unix domain sockets (for SmartOS zones) and serial ports
//! (for KVM/HVM guests on Unix and Windows). Platform detection
//! automatically selects the appropriate transport.
//!
//! The socket transport uses safe Rust I/O (`UnixStream`) exclusively.
//! The serial transport requires `unsafe` only for terminal
//! configuration (termios), file locking (fcntl), poll-based timeouts,
//! and Windows serial port setup — operations with no safe Rust
//! equivalent.

use std::io;
use std::path::PathBuf;

#[cfg(unix)]
mod unix;

#[cfg(windows)]
mod windows;

/// Errors specific to the transport layer.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("timed out waiting for response")]
    Timeout,
    #[error("connection closed unexpectedly")]
    Eof,
    #[error("invalid UTF-8 in response")]
    InvalidData,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Interface for metadata protocol transports.
///
/// Implemented by the platform-specific `Transport` and by
/// `MockTransport` in tests.
pub trait MetadataTransport {
    fn send(&self, data: &str) -> Result<(), TransportError>;
    fn recv_line(&self, timeout_ms: u64) -> Result<String, TransportError>;
    fn reconnect(&mut self) -> anyhow::Result<()>;
    fn is_serial(&self) -> bool;
}

/// Detected transport configuration.
#[derive(Clone, Debug)]
pub enum TransportConfig {
    /// Unix domain socket (SmartOS zone).
    #[cfg(unix)]
    UnixSocket(PathBuf),
    /// Serial port (KVM/HVM guest).
    Serial(PathBuf),
}

/// Line-oriented transport for the metadata protocol.
///
/// Uses safe Rust I/O for sockets. Serial ports require minimal
/// unsafe for terminal configuration and poll-based timeouts.
pub struct Transport {
    config: TransportConfig,
    #[cfg(unix)]
    inner: unix::TransportInner,
    #[cfg(windows)]
    file: std::fs::File,
}

impl MetadataTransport for Transport {
    fn send(&self, data: &str) -> Result<(), TransportError> {
        Transport::send(self, data)
    }

    fn recv_line(&self, timeout_ms: u64) -> Result<String, TransportError> {
        Transport::recv_line(self, timeout_ms)
    }

    fn reconnect(&mut self) -> anyhow::Result<()> {
        Transport::reconnect(self)
    }

    fn is_serial(&self) -> bool {
        matches!(self.config, TransportConfig::Serial(_))
    }
}
