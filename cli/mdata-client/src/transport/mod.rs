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

/// Detected transport configuration.
#[derive(Clone, Debug)]
pub enum TransportConfig {
    /// Unix domain socket (SmartOS zone).
    #[cfg(unix)]
    UnixSocket(PathBuf),
    /// Serial port (KVM/HVM guest).
    Serial(PathBuf),
}

/// Low-level transport for sending and receiving lines.
pub struct Transport {
    config: TransportConfig,
    #[cfg(unix)]
    fd: std::os::unix::io::RawFd,
    #[cfg(windows)]
    handle: windows::RawHandle,
}

impl Transport {
    /// Whether this transport is a serial port.
    pub fn is_serial(&self) -> bool {
        matches!(self.config, TransportConfig::Serial(_))
    }
}
