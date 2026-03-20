// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Unix transport implementation.
//!
//! Socket transport (SmartOS zones) uses safe Rust I/O exclusively.
//! Serial transport (KVM/HVM guests) uses safe I/O for read/write
//! and requires unsafe only for:
//! - `poll()` — timeout-based readability check (no safe equivalent
//!   for `File`)
//! - `tcgetattr`/`tcsetattr` — terminal raw mode configuration
//! - `fcntl(F_SETLK)` — exclusive file locking
//! - `fcntl(F_SETFL)` — clearing O_NONBLOCK after setup
//! - `tcflush` — flushing pending serial data

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::debug;

use super::{Transport, TransportConfig, TransportError};

/// Platform-specific transport inner type.
pub(super) enum TransportInner {
    Socket(UnixStream),
    Serial(File),
}

impl Transport {
    /// Detect the appropriate transport and open it.
    pub fn open() -> Result<Self> {
        let config = detect_transport()?;
        let inner = open_transport(&config)?;
        Ok(Self { config, inner })
    }

    /// Send a string over the transport.
    pub fn send(&self, data: &str) -> Result<(), TransportError> {
        let bytes = data.as_bytes();
        match &self.inner {
            TransportInner::Socket(stream) => {
                (&*stream).write_all(bytes).map_err(TransportError::Io)
            }
            TransportInner::Serial(file) => (&*file).write_all(bytes).map_err(TransportError::Io),
        }
    }

    /// Receive a single line (terminated by `\n`) with a timeout.
    ///
    /// Returns the line content without the trailing newline.
    pub fn recv_line(&self, timeout_ms: u64) -> Result<String, TransportError> {
        match &self.inner {
            TransportInner::Socket(stream) => recv_line_socket(stream, timeout_ms),
            TransportInner::Serial(file) => recv_line_serial(file, timeout_ms),
        }
    }

    /// Close and reopen the transport for protocol reset.
    pub fn reconnect(&mut self) -> Result<()> {
        // Dropping the old inner closes the fd automatically.
        self.inner = open_transport(&self.config)?;
        Ok(())
    }
}

// No custom Drop needed — UnixStream and File close their fds on drop.

/// Receive a line from a Unix socket using `set_read_timeout`.
///
/// Fully safe — no unsafe required.
fn recv_line_socket(stream: &UnixStream, timeout_ms: u64) -> Result<String, TransportError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut line = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TransportError::Timeout);
        }

        stream
            .set_read_timeout(Some(remaining))
            .map_err(TransportError::Io)?;

        match (&*stream).read(&mut byte) {
            Ok(0) => return Err(TransportError::Eof),
            Ok(_) => {
                if byte[0] == b'\n' {
                    return String::from_utf8(line).map_err(|_| TransportError::InvalidData);
                }
                line.push(byte[0]);
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                return Err(TransportError::Timeout);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                continue;
            }
            Err(e) => return Err(TransportError::Io(e)),
        }
    }
}

/// Receive a line from a serial port using `poll()` for timeouts.
///
/// `File` has no `set_read_timeout`, so we use `poll()` to wait for
/// readability before each `read()` call.
fn recv_line_serial(file: &File, timeout_ms: u64) -> Result<String, TransportError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut line = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TransportError::Timeout);
        }

        let remaining_ms = remaining.as_millis().min(i32::MAX as u128) as i32;

        if !poll_readable(file.as_raw_fd(), remaining_ms)? {
            return Err(TransportError::Timeout);
        }

        match (&*file).read(&mut byte) {
            Ok(0) => return Err(TransportError::Eof),
            Ok(_) => {
                if byte[0] == b'\n' {
                    return String::from_utf8(line).map_err(|_| TransportError::InvalidData);
                }
                line.push(byte[0]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                continue;
            }
            Err(e) => return Err(TransportError::Io(e)),
        }
    }
}

/// Poll a file descriptor for readability with a timeout.
fn poll_readable(fd: RawFd, timeout_ms: i32) -> Result<bool, TransportError> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: pfd is a stack-allocated pollfd struct with a valid
        // fd obtained from File::as_raw_fd(). nfds is 1, matching
        // the single pollfd. timeout_ms is bounded by i32::MAX.
        let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if ret < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(TransportError::Io(err));
        }
        if ret == 0 {
            return Ok(false);
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(TransportError::Io(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "poll returned error condition",
            )));
        }
        return Ok(true);
    }
}

/// Detect the appropriate transport for this platform.
fn detect_transport() -> Result<TransportConfig> {
    // SmartOS zone socket paths (tried in order)
    let socket_paths = [
        "/.zonecontrol/metadata.sock",
        "/native/.zonecontrol/metadata.sock",
        "/var/run/smartdc/metadata.sock",
    ];

    for path in &socket_paths {
        if Path::new(path).exists() {
            debug!("detected unix socket transport: {path}");
            return Ok(TransportConfig::UnixSocket(PathBuf::from(path)));
        }
    }

    // Serial ports for KVM/HVM guests
    let serial_paths = [
        "/dev/term/b", // illumos/SmartOS
        "/dev/ttyS1",  // Linux
        "/dev/tty01",  // NetBSD
        "/dev/cua01",  // OpenBSD
        "/dev/cuau1",  // FreeBSD
    ];

    for path in &serial_paths {
        if Path::new(path).exists() {
            debug!("detected serial transport: {path}");
            return Ok(TransportConfig::Serial(PathBuf::from(path)));
        }
    }

    bail!(
        "no metadata transport found; tried sockets ({}) \
         and serial ports ({})",
        socket_paths.join(", "),
        serial_paths.join(", "),
    )
}

/// Open the detected transport.
fn open_transport(config: &TransportConfig) -> Result<TransportInner> {
    match config {
        TransportConfig::UnixSocket(path) => {
            let stream = UnixStream::connect(path)
                .with_context(|| format!("connecting to metadata socket: {}", path.display()))?;
            Ok(TransportInner::Socket(stream))
        }
        TransportConfig::Serial(path) => {
            let file = open_serial(path)?;
            Ok(TransportInner::Serial(file))
        }
    }
}

/// Open and configure a serial port.
///
/// Uses `std::fs::File` for the open; unsafe is limited to terminal
/// configuration and locking (no safe Rust equivalent).
fn open_serial(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)
        .map_err(|err| {
            if err.kind() == io::ErrorKind::PermissionDenied {
                anyhow::anyhow!(
                    "permission denied opening {}: \
                     are you running as root?",
                    path.display()
                )
            } else {
                anyhow::anyhow!("opening serial port {}: {}", path.display(), err)
            }
        })?;

    let fd = file.as_raw_fd();

    // Acquire an exclusive lock to prevent concurrent access.
    // If this fails, `file` is dropped which closes the fd.
    acquire_exclusive_lock(fd, path)?;

    // Configure raw mode (no echo, no canonical, 8-bit, etc.).
    // If this fails, `file` is dropped which closes the fd.
    configure_serial_raw(fd)?;

    // Clear O_NONBLOCK now that setup is done.
    // SAFETY: fd is valid (owned by `file`). F_GETFL/F_SETFL
    // only modify the file status flags on our own descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    }

    // Flush any pending data from previous sessions.
    // SAFETY: fd is valid, flushing both input and output queues.
    unsafe { libc::tcflush(fd, libc::TCIOFLUSH) };

    Ok(file)
}

/// Acquire an exclusive (F_WRLCK) lock on the serial port fd.
fn acquire_exclusive_lock(fd: RawFd, path: &Path) -> Result<()> {
    // SAFETY: libc::flock is a plain C struct; all-zeros is a valid
    // initial state (no lock, offset 0, length 0).
    let mut flock_val: libc::flock = unsafe { std::mem::zeroed() };
    #[allow(clippy::unnecessary_cast)]
    {
        flock_val.l_type = libc::F_WRLCK as i16;
    }
    flock_val.l_whence = libc::SEEK_SET as i16;

    // SAFETY: fd is valid (owned by caller's File), flock_val is
    // properly initialized. F_SETLK is a non-blocking lock attempt.
    if unsafe { libc::fcntl(fd, libc::F_SETLK, &flock_val) } < 0 {
        let err = io::Error::last_os_error();
        bail!(
            "failed to lock serial port {} \
             (another mdata process may be running): {}",
            path.display(),
            err,
        );
    }

    Ok(())
}

/// Configure a serial port for raw (non-canonical) I/O.
fn configure_serial_raw(fd: RawFd) -> Result<()> {
    // SAFETY: libc::termios is a plain C struct; all-zeros is valid.
    let mut tios: libc::termios = unsafe { std::mem::zeroed() };

    // SAFETY: fd is valid, tios is a properly sized stack buffer.
    if unsafe { libc::tcgetattr(fd, &mut tios) } < 0 {
        bail!("tcgetattr failed: {}", io::Error::last_os_error());
    }

    tios.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
    tios.c_oflag &= !libc::OPOST;
    tios.c_cflag |= libc::CS8;
    tios.c_cflag &= !libc::HUPCL;
    tios.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
    tios.c_cc[libc::VMIN] = 0;
    tios.c_cc[libc::VTIME] = 1;

    // SAFETY: fd is valid, tios is properly initialized from
    // tcgetattr + our modifications. TCSAFLUSH drains output
    // and discards pending input before applying.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &tios) } < 0 {
        bail!("tcsetattr failed: {}", io::Error::last_os_error());
    }

    Ok(())
}
