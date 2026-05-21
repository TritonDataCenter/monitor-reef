// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Async client for bhyve's in-zone Unix control socket.
//!
//! Ported from the legacy `vmm-migrate-agent::bhyve_ctl` blocking
//! `std::os::unix::net::UnixStream` client to `tokio::net::UnixStream`
//! so it composes with the state machines (which are themselves
//! tokio tasks). The wire protocol — line-delimited JSON commands,
//! a single JSON response line, optionally followed by binary
//! payload for `export-state` / `import-state` — is unchanged.
//!
//! bhyve's control socket listener is single-threaded inside the VM
//! zone. The connection is kept alive for the duration of one
//! migration so we don't deadlock on reconnect while a prior fgets
//! is still blocking on EOF (an issue we hit in the legacy code).
//!
//! Only used by the SmartOS-side agent. On dev machines (macOS /
//! Linux) the module still compiles — `tokio::net::UnixStream`
//! exists on every unix — but the bhyve binary that backs it does
//! not, so any actual `connect` will fail with `ENOENT`.

use std::io;
use std::path::Path;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// One JSON reply line bhyve writes back. Optional fields cover
/// every command shape; we deserialise into one struct + check the
/// `success` boolean.
#[derive(Deserialize, Debug)]
struct Response {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    /// Returned by the legacy `export-devices` command. Not used
    /// by the LM-2 surface, but kept on the struct so a future
    /// migration that exports devices separately doesn't need to
    /// change the response shape.
    #[serde(default)]
    #[allow(dead_code)]
    len: Option<u64>,
    #[serde(default)]
    kern_len: Option<u64>,
    #[serde(default)]
    dev_len: Option<u64>,
    #[serde(default)]
    ncpus: Option<u32>,
    #[serde(default)]
    lowmem: Option<usize>,
    #[serde(default)]
    highmem: Option<usize>,
}

/// VM status info returned by the `status` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmStatus {
    pub num_cpus: u32,
    pub lowmem_size: usize,
    pub highmem_size: usize,
}

/// Async client handle for one in-zone bhyve control socket.
pub struct BhyveCtl {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl BhyveCtl {
    /// Connect to a bhyve control socket. Path is typically
    /// `/zones/<uuid>/root/tmp/bhyve.sock`.
    pub async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read_half),
            writer: write_half,
        })
    }

    /// Query VM status — number of vCPUs and memory region sizes.
    pub async fn status(&mut self) -> io::Result<VmStatus> {
        self.send_command(r#"{"command":"status"}"#).await?;
        let resp = self.read_response().await?;
        if !resp.success {
            return Err(io::Error::other(
                resp.error.unwrap_or_else(|| "status failed".into()),
            ));
        }
        Ok(VmStatus {
            num_cpus: resp.ncpus.unwrap_or(1),
            lowmem_size: resp.lowmem.unwrap_or(0),
            highmem_size: resp.highmem.unwrap_or(0),
        })
    }

    /// Pause viona rings. Must be called BEFORE [`Self::pause_vm`]
    /// so the kernel ring workers stop draining the guest's avail
    /// rings while we capture RAM.
    pub async fn pause_devices(&mut self) -> io::Result<()> {
        self.simple_cmd(r#"{"command":"pause-devices"}"#).await
    }

    /// Pause vCPUs + device timers. The legacy comment is worth
    /// preserving verbatim: we do NOT pause from the GZ via
    /// `VM_PAUSE` ioctl because it deadlocks against the subsequent
    /// `VM_DATA_WRITE` (the kernel's `vcpu_lock_one` blocks on vCPUs
    /// in `VM_RUN`). Doing it via bhyve's control socket lets
    /// userspace pause its own threads cleanly.
    pub async fn pause_vm(&mut self) -> io::Result<()> {
        self.simple_cmd(r#"{"command":"pause-vm"}"#).await
    }

    /// Drain in-flight block-device I/O AFTER [`Self::pause_vm`].
    /// Ensures captured state has every used-ring entry and status
    /// byte committed for in-flight requests; otherwise the migrated
    /// state has descriptors that look "consumed" on the destination
    /// but never see their completions, hanging guest disk I/O.
    pub async fn drain_devices(&mut self) -> io::Result<()> {
        self.simple_cmd(r#"{"command":"drain-devices"}"#).await
    }

    /// Resume vCPUs + device timers. Target-side only; called after
    /// [`import_state`] completes.
    pub async fn resume_vm(&mut self) -> io::Result<()> {
        self.simple_cmd(r#"{"command":"resume-vm"}"#).await
    }

    /// Resume viona rings. Target-side only; pairs with
    /// [`Self::pause_devices`].
    pub async fn resume_devices(&mut self) -> io::Result<()> {
        self.simple_cmd(r#"{"command":"resume-devices"}"#).await
    }

    /// Export ALL state: kernel (VMM_TIME, system devices, per-vCPU)
    /// + bhyve userspace devices (PCI, virtio, viona). Two packed
    /// nvlists, opaque to us. Must be called after `pause_devices`
    /// + `pause_vm` + `drain_devices`.
    pub async fn export_state(&mut self) -> io::Result<(Vec<u8>, Vec<u8>)> {
        self.send_command(r#"{"command":"export-state"}"#).await?;
        let resp = self.read_response().await?;
        if !resp.success {
            return Err(io::Error::other(
                resp.error.unwrap_or_else(|| "export-state failed".into()),
            ));
        }
        let kern_len = resp
            .kern_len
            .ok_or_else(|| io::Error::other("missing kern_len"))? as usize;
        let dev_len = resp
            .dev_len
            .ok_or_else(|| io::Error::other("missing dev_len"))? as usize;

        let mut kern = vec![0u8; kern_len];
        self.reader.read_exact(&mut kern).await?;
        let mut dev = vec![0u8; dev_len];
        self.reader.read_exact(&mut dev).await?;
        Ok((kern, dev))
    }

    /// Full state import: kernel state nvlist + device state
    /// nvlist. bhyve reads destination VMM_TIME live and applies
    /// the cross-host adjustment internally — the source's exported
    /// VMM_TIME blob is informational on this side.
    pub async fn import_state(&mut self, kern_data: &[u8], dev_data: &[u8]) -> io::Result<()> {
        let cmd = format!(
            "{{\"command\":\"import-state\",\"kern_len\":{},\"dev_len\":{}}}",
            kern_data.len(),
            dev_data.len(),
        );
        self.send_command(&cmd).await?;
        self.writer.write_all(kern_data).await?;
        self.writer.write_all(dev_data).await?;
        let resp = self.read_response().await?;
        if resp.success {
            Ok(())
        } else {
            Err(io::Error::other(
                resp.error.unwrap_or_else(|| "import-state failed".into()),
            ))
        }
    }

    /// Send a command and check the response is a bare-success
    /// `{"success": true}`. Helper for the side-effect-only
    /// commands.
    async fn simple_cmd(&mut self, cmd: &str) -> io::Result<()> {
        self.send_command(cmd).await?;
        let resp = self.read_response().await?;
        if resp.success {
            Ok(())
        } else {
            Err(io::Error::other(
                resp.error.unwrap_or_else(|| "command failed".into()),
            ))
        }
    }

    async fn send_command(&mut self, cmd: &str) -> io::Result<()> {
        self.writer.write_all(cmd.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn read_response(&mut self) -> io::Result<Response> {
        // Read a single line. `BufReader::read_line` stops at `\n`
        // and leaves any subsequent bytes (including the binary
        // export blobs that follow `export-state`) untouched in the
        // internal buffer + on the wire, which is exactly what
        // we want — the next `read_exact` for the kern/dev nvlists
        // can pick them up unbuffered.
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "control socket closed",
            ));
        }
        serde_json::from_str(line.trim_end())
            .map_err(|e| io::Error::other(format!("JSON parse error: {e}")))
    }
}
