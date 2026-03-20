// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Windows transport implementation.
//!
//! Communicates with the metadata service over COM2 serial port,
//! matching the transport used by the original mdata-get.exe from
//! sdc-vmtools.
//!
//! Uses `std::fs::File` for open/close/read/write (safe Rust I/O).
//! Unsafe is limited to Win32 serial port configuration APIs
//! (GetCommState, SetCommState, SetCommTimeouts, PurgeComm) which
//! have no safe Rust equivalent.
//!
//! Win32 FFI types (Dcb, CommTimeouts) are defined inline rather than
//! pulling in the `windows` crate — we only need 4 functions and the
//! crate adds ~50 MB of bindings.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::{Transport, TransportConfig, TransportError};

/// Opaque handle type matching Windows HANDLE.
type RawHandle = *mut std::ffi::c_void;

// ── Win32 FFI definitions ──────────────────────────────────────

/// DCB flags bitmask: only fBinary set (bit 0).
const DCB_FLAGS_BINARY: u32 = 0x0001;

#[repr(C)]
struct Dcb {
    dcb_length: u32,
    baud_rate: u32,
    flags: u32,
    w_reserved: u16,
    xon_lim: u16,
    xoff_lim: u16,
    byte_size: u8,
    parity: u8,
    stop_bits: u8,
    xon_char: i8,
    xoff_char: i8,
    error_char: i8,
    eof_char: i8,
    evt_char: i8,
    w_reserved1: u16,
}

#[repr(C)]
struct CommTimeouts {
    read_interval_timeout: u32,
    read_total_timeout_multiplier: u32,
    read_total_timeout_constant: u32,
    write_total_timeout_multiplier: u32,
    write_total_timeout_constant: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCommState(h_file: RawHandle, lp_dcb: *mut Dcb) -> i32;
    fn SetCommState(h_file: RawHandle, lp_dcb: *mut Dcb) -> i32;

    fn SetCommTimeouts(h_file: RawHandle, lp_comm_timeouts: *const CommTimeouts) -> i32;

    fn PurgeComm(h_file: RawHandle, dw_flags: u32) -> i32;
}

/// PURGE_RXCLEAR | PURGE_TXCLEAR
const PURGE_RX_TX: u32 = 0x0004 | 0x0008;

// ── Transport implementation ───────────────────────────────────

impl Transport {
    /// Open the COM2 serial port.
    ///
    /// On Windows, this always uses COM2 (the virtual serial port
    /// connected to the SmartOS metadata service).
    pub fn open() -> Result<Self> {
        let config = TransportConfig::Serial(PathBuf::from("\\\\.\\COM2"));
        let file = open_serial_port(&config)?;
        Ok(Self { config, file })
    }

    /// Send a string over the transport.
    pub fn send(&self, data: &str) -> Result<(), TransportError> {
        (&self.file)
            .write_all(data.as_bytes())
            .map_err(TransportError::Io)
    }

    /// Receive a single line (terminated by `\n`) with a timeout.
    ///
    /// Uses SetCommTimeouts to set the read deadline, then safe
    /// `File::read` for the actual I/O.
    pub fn recv_line(&self, timeout_ms: u64) -> Result<String, TransportError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut line = Vec::new();
        let mut byte = [0u8; 1];

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::Timeout);
            }

            let remaining_ms = remaining.as_millis().min(u32::MAX as u128) as u32;

            // Set read timeout to remaining time.
            // SAFETY: handle is valid (owned by self.file),
            // timeouts is a properly initialized stack struct.
            let timeouts = CommTimeouts {
                read_interval_timeout: 0,
                read_total_timeout_multiplier: 0,
                read_total_timeout_constant: remaining_ms,
                write_total_timeout_multiplier: 0,
                write_total_timeout_constant: 5000,
            };
            let handle = self.file.as_raw_handle() as RawHandle;
            if unsafe { SetCommTimeouts(handle, &timeouts) } == 0 {
                return Err(TransportError::Io(io::Error::last_os_error()));
            }

            let n = (&self.file).read(&mut byte).map_err(TransportError::Io)?;

            if n == 0 {
                return Err(TransportError::Timeout);
            }

            if byte[0] == b'\n' {
                return String::from_utf8(line).map_err(|_| TransportError::InvalidData);
            }
            line.push(byte[0]);
        }
    }

    /// Close and reopen the transport for protocol reset.
    pub fn reconnect(&mut self) -> Result<()> {
        // Dropping the old file closes the handle automatically.
        self.file = open_serial_port(&self.config)?;
        Ok(())
    }
}

// No custom Drop needed — File closes the handle on drop.

/// Open and configure the serial port for metadata communication.
fn open_serial_port(config: &TransportConfig) -> Result<File> {
    let TransportConfig::Serial(path) = config;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0) // exclusive access
        .open(path)
        .map_err(
            |err| anyhow::anyhow!("failed to open serial port {}: {}", path.display(), err,),
        )?;

    // Configure serial port: 8N1, no flow control.
    // If this fails, `file` is dropped which closes the handle.
    configure_serial(&file)?;

    // Flush any pending data from previous sessions.
    // SAFETY: handle is valid (owned by file), PURGE_RX_TX clears
    // both input and output buffers.
    let handle = file.as_raw_handle() as RawHandle;
    unsafe { PurgeComm(handle, PURGE_RX_TX) };

    Ok(file)
}

/// Configure serial port for raw 8N1 communication.
fn configure_serial(file: &File) -> Result<()> {
    let handle = file.as_raw_handle() as RawHandle;

    // SAFETY: Dcb is a plain C struct; all-zeros is valid.
    let mut dcb: Dcb = unsafe { std::mem::zeroed() };
    dcb.dcb_length = std::mem::size_of::<Dcb>() as u32;

    // SAFETY: handle is valid, dcb is a properly sized stack buffer.
    if unsafe { GetCommState(handle, &mut dcb) } == 0 {
        bail!("GetCommState failed: {}", io::Error::last_os_error());
    }

    // 8 data bits, no parity, 1 stop bit, binary mode, no flow control
    dcb.byte_size = 8;
    dcb.parity = 0; // NOPARITY
    dcb.stop_bits = 0; // ONESTOPBIT
    dcb.flags = DCB_FLAGS_BINARY;

    // SAFETY: handle is valid, dcb is properly initialized from
    // GetCommState + our modifications.
    if unsafe { SetCommState(handle, &mut dcb) } == 0 {
        bail!("SetCommState failed: {}", io::Error::last_os_error());
    }

    // Set initial timeouts.
    // SAFETY: handle is valid, timeouts is a properly initialized
    // stack struct.
    let timeouts = CommTimeouts {
        read_interval_timeout: 0,
        read_total_timeout_multiplier: 0,
        read_total_timeout_constant: 6000,
        write_total_timeout_multiplier: 0,
        write_total_timeout_constant: 5000,
    };

    if unsafe { SetCommTimeouts(handle, &timeouts) } == 0 {
        bail!("SetCommTimeouts failed: {}", io::Error::last_os_error());
    }

    Ok(())
}
