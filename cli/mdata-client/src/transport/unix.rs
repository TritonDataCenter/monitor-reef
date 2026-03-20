// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Unix transport implementation.
//!
//! Supports Unix domain sockets (SmartOS zones) and serial ports
//! (KVM/HVM guests) using poll() for timeout-based I/O.

use std::io;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::{Transport, TransportConfig, TransportError};

impl Transport {
    /// Detect the appropriate transport and open it.
    pub fn open() -> Result<Self> {
        let config = detect_transport()?;
        let fd = open_transport(&config)?;
        Ok(Self { config, fd })
    }

    /// Send a string over the transport.
    pub fn send(&self, data: &str) -> Result<(), TransportError> {
        let bytes = data.as_bytes();
        let mut written = 0;
        while written < bytes.len() {
            let n = unsafe {
                libc::write(
                    self.fd,
                    bytes[written..].as_ptr() as *const libc::c_void,
                    bytes.len() - written,
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(TransportError::Io(err));
            }
            written += n as usize;
        }
        Ok(())
    }

    /// Receive a single line (terminated by `\n`) with a timeout.
    ///
    /// Returns the line content without the trailing newline.
    pub fn recv_line(&self, timeout_ms: u64) -> Result<String, TransportError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut line = Vec::new();
        let mut byte = [0u8; 1];

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::Timeout);
            }

            let remaining_ms = remaining.as_millis().min(i32::MAX as u128) as i32;

            if !poll_readable(self.fd, remaining_ms)? {
                return Err(TransportError::Timeout);
            }

            let n = unsafe { libc::read(self.fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };

            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(TransportError::Io(err));
            }
            if n == 0 {
                return Err(TransportError::Eof);
            }

            if byte[0] == b'\n' {
                return String::from_utf8(line).map_err(|_| TransportError::InvalidData);
            }
            line.push(byte[0]);
        }
    }

    /// Close and reopen the transport for protocol reset.
    pub fn reconnect(&mut self) -> Result<()> {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
        self.fd = open_transport(&self.config)?;
        Ok(())
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.fd = -1;
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

/// Open the transport, returning the raw file descriptor.
fn open_transport(config: &TransportConfig) -> Result<RawFd> {
    match config {
        TransportConfig::UnixSocket(path) => open_socket(path),
        TransportConfig::Serial(path) => open_serial(path),
    }
}

/// Connect to a Unix domain socket.
fn open_socket(path: &Path) -> Result<RawFd> {
    let stream = UnixStream::connect(path)
        .with_context(|| format!("connecting to metadata socket: {}", path.display()))?;
    Ok(stream.into_raw_fd())
}

/// Open and configure a serial port.
fn open_serial(path: &Path) -> Result<RawFd> {
    let c_path = std::ffi::CString::new(
        path.to_str()
            .context("serial port path is not valid UTF-8")?,
    )
    .context("serial port path contains null byte")?;

    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::PermissionDenied {
            bail!(
                "permission denied opening {}: \
                 are you running as root?",
                path.display()
            );
        }
        bail!("opening serial port {}: {}", path.display(), err);
    }

    // Acquire an exclusive lock
    let mut flock_val: libc::flock = unsafe { std::mem::zeroed() };
    #[allow(clippy::unnecessary_cast)]
    {
        flock_val.l_type = libc::F_WRLCK as i16;
    }
    flock_val.l_whence = libc::SEEK_SET as i16;
    flock_val.l_start = 0;
    flock_val.l_len = 0;

    if unsafe { libc::fcntl(fd, libc::F_SETLK, &flock_val) } < 0 {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        bail!(
            "failed to lock serial port {} \
             (another mdata process may be running): {}",
            path.display(),
            err,
        );
    }

    // Configure raw mode
    if let Err(e) = configure_serial_raw(fd) {
        unsafe { libc::close(fd) };
        return Err(e);
    }

    // Clear O_NONBLOCK now that setup is done
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    }

    // Flush any pending data
    unsafe { libc::tcflush(fd, libc::TCIOFLUSH) };

    Ok(fd)
}

/// Configure a serial port for raw (non-canonical) I/O.
fn configure_serial_raw(fd: RawFd) -> Result<()> {
    let mut tios: libc::termios = unsafe { std::mem::zeroed() };

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

    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &tios) } < 0 {
        bail!("tcsetattr failed: {}", io::Error::last_os_error());
    }

    Ok(())
}
