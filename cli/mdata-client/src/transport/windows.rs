// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Windows transport implementation.
//!
//! Communicates with the metadata service over COM2 serial port,
//! matching the transport used by the original mdata-get.exe from
//! sdc-vmtools. Uses Win32 serial API (CreateFileW, SetCommState,
//! SetCommTimeouts, ReadFile, WriteFile).
//!
//! Win32 FFI types (Dcb, CommTimeouts) are defined inline rather than
//! pulling in the `windows` crate — we only need 5 functions and the
//! crate adds ~50 MB of bindings.

use std::io;
use std::path::PathBuf;
use std::ptr;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::{Transport, TransportConfig, TransportError};

/// Opaque handle type matching Windows HANDLE.
pub(super) type RawHandle = *mut std::ffi::c_void;

// ── Win32 FFI definitions ──────────────────────────────────────

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const INVALID_HANDLE_VALUE: RawHandle = -1isize as RawHandle;

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
    fn CreateFileW(
        lp_file_name: *const u16,
        dw_desired_access: u32,
        dw_share_mode: u32,
        lp_security_attributes: *mut std::ffi::c_void,
        dw_creation_disposition: u32,
        dw_flags_and_attributes: u32,
        h_template_file: RawHandle,
    ) -> RawHandle;

    fn CloseHandle(h_object: RawHandle) -> i32;

    fn ReadFile(
        h_file: RawHandle,
        lp_buffer: *mut u8,
        n_number_of_bytes_to_read: u32,
        lp_number_of_bytes_read: *mut u32,
        lp_overlapped: *mut std::ffi::c_void,
    ) -> i32;

    fn WriteFile(
        h_file: RawHandle,
        lp_buffer: *const u8,
        n_number_of_bytes_to_write: u32,
        lp_number_of_bytes_written: *mut u32,
        lp_overlapped: *mut std::ffi::c_void,
    ) -> i32;

    fn GetCommState(h_file: RawHandle, lp_dcb: *mut Dcb) -> i32;
    fn SetCommState(h_file: RawHandle, lp_dcb: *mut Dcb) -> i32;

    fn SetCommTimeouts(h_file: RawHandle, lp_comm_timeouts: *const CommTimeouts) -> i32;

    fn PurgeComm(h_file: RawHandle, dw_flags: u32) -> i32;
}

/// PURGE_RXCLEAR | PURGE_TXCLEAR
const PURGE_RX_TX: u32 = 0x0004 | 0x0008;

// ── Transport implementation ───────────────────────────────────

impl Transport {
    /// Detect the appropriate transport and open it.
    ///
    /// On Windows, this always uses COM2 (the virtual serial port
    /// connected to the SmartOS metadata service).
    pub fn open() -> Result<Self> {
        let config = TransportConfig::Serial(PathBuf::from("\\\\.\\COM2"));
        let handle = open_serial_port(&config)?;
        Ok(Self { config, handle })
    }

    /// Send a string over the transport.
    pub fn send(&self, data: &str) -> Result<(), TransportError> {
        let bytes = data.as_bytes();
        let mut written = 0u32;
        let mut total = 0usize;
        while total < bytes.len() {
            let to_write = (bytes.len() - total).min(u32::MAX as usize) as u32;
            let ret = unsafe {
                WriteFile(
                    self.handle,
                    bytes[total..].as_ptr(),
                    to_write,
                    &mut written,
                    ptr::null_mut(),
                )
            };
            if ret == 0 {
                return Err(TransportError::Io(io::Error::last_os_error()));
            }
            total += written as usize;
        }
        Ok(())
    }

    /// Receive a single line (terminated by `\n`) with a timeout.
    ///
    /// Uses SetCommTimeouts to enforce the deadline. ReadFile returns
    /// 0 bytes read when the timeout expires.
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

            // Set read timeout to remaining time
            let timeouts = CommTimeouts {
                read_interval_timeout: 0,
                read_total_timeout_multiplier: 0,
                read_total_timeout_constant: remaining_ms,
                write_total_timeout_multiplier: 0,
                write_total_timeout_constant: 5000,
            };
            if unsafe { SetCommTimeouts(self.handle, &timeouts) } == 0 {
                return Err(TransportError::Io(io::Error::last_os_error()));
            }

            let mut bytes_read = 0u32;
            let ret = unsafe {
                ReadFile(
                    self.handle,
                    byte.as_mut_ptr(),
                    1,
                    &mut bytes_read,
                    ptr::null_mut(),
                )
            };

            if ret == 0 {
                return Err(TransportError::Io(io::Error::last_os_error()));
            }
            if bytes_read == 0 {
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
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.handle) };
            self.handle = INVALID_HANDLE_VALUE;
        }
        self.handle = open_serial_port(&self.config)?;
        Ok(())
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.handle) };
            self.handle = INVALID_HANDLE_VALUE;
        }
    }
}

/// Encode a Rust string as a null-terminated UTF-16 wide string.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Open and configure the serial port for metadata communication.
fn open_serial_port(config: &TransportConfig) -> Result<RawHandle> {
    let TransportConfig::Serial(path) = config;

    let path_str = path.to_str().unwrap_or("\\\\.\\COM2");
    let wide_path = to_wide(path_str);

    let handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0, // exclusive access
            ptr::null_mut(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        bail!("failed to open serial port {}: {}", path_str, err,);
    }

    // Configure serial port: 8N1, no flow control
    if let Err(e) = configure_serial(handle) {
        unsafe { CloseHandle(handle) };
        return Err(e);
    }

    // Flush any pending data
    unsafe { PurgeComm(handle, PURGE_RX_TX) };

    Ok(handle)
}

/// Configure serial port for raw 8N1 communication.
fn configure_serial(handle: RawHandle) -> Result<()> {
    let mut dcb: Dcb = unsafe { std::mem::zeroed() };
    dcb.dcb_length = std::mem::size_of::<Dcb>() as u32;

    if unsafe { GetCommState(handle, &mut dcb) } == 0 {
        bail!("GetCommState failed: {}", io::Error::last_os_error());
    }

    // 8 data bits, no parity, 1 stop bit, binary mode, no flow control
    dcb.byte_size = 8;
    dcb.parity = 0; // NOPARITY
    dcb.stop_bits = 0; // ONESTOPBIT
    dcb.flags = DCB_FLAGS_BINARY;

    if unsafe { SetCommState(handle, &mut dcb) } == 0 {
        bail!("SetCommState failed: {}", io::Error::last_os_error());
    }

    // Set initial timeouts
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
