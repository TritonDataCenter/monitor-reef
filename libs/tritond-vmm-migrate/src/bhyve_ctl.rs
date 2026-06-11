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

/// One JSON reply line the deployed bhyve writes back. The envelope
/// is `{"status":"ok",...}` on success and `{"status":"error",
/// "msg":"..."}` on failure (NOT the legacy `{"success":bool}` the
/// donor agent used — the deployed PI's `bhyve_control.c` speaks a
/// single-blob `status` protocol). Optional fields cover every
/// command shape.
#[derive(Deserialize, Debug)]
struct Response {
    status: String,
    #[serde(default)]
    msg: Option<String>,
    /// `export-state` reports the single combined state-blob length.
    #[serde(default)]
    blob_len: Option<u64>,
    #[serde(default)]
    ncpus: Option<u32>,
    #[serde(default)]
    lowmem: Option<usize>,
    #[serde(default)]
    highmem: Option<usize>,
}

impl Response {
    fn is_ok(&self) -> bool {
        self.status == "ok"
    }
    fn err_msg(&self) -> String {
        self.msg
            .clone()
            .unwrap_or_else(|| format!("bhyve returned status={:?}", self.status))
    }
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
        if !resp.is_ok() {
            return Err(io::Error::other(resp.err_msg()));
        }
        Ok(VmStatus {
            num_cpus: resp.ncpus.unwrap_or(1),
            lowmem_size: resp.lowmem.unwrap_or(0),
            highmem_size: resp.highmem.unwrap_or(0),
        })
    }

    /// Pause the guest. The deployed bhyve's single `pause` command
    /// pauses vCPUs first then devices internally (it owns the
    /// ordering — see `bhyve_control.c::cmd_pause`), so the agent no
    /// longer issues the donor's separate pause-devices/pause-vm/
    /// drain-devices trio. We pause via the control socket rather
    /// than a GZ `VM_PAUSE` ioctl because the latter deadlocks
    /// against `VM_DATA_WRITE` (`vcpu_lock_one` blocks on vCPUs in
    /// `VM_RUN`).
    pub async fn pause(&mut self) -> io::Result<()> {
        self.simple_cmd(r#"{"command":"pause"}"#).await
    }

    /// Resume the guest. The deployed bhyve's single `resume` resumes
    /// devices first then vCPUs internally. Used on the source for a
    /// pre-cutover abort, and on the target after `import_state`.
    pub async fn resume(&mut self) -> io::Result<()> {
        self.simple_cmd(r#"{"command":"resume"}"#).await
    }

    /// Export ALL migration state as a single opaque blob (kernel
    /// VMM_TIME + system devices + per-vCPU + bhyve userspace
    /// devices, packed by `build_save_stream`). Must be called after
    /// [`Self::pause`]. The reply is `{"status":"ok","blob_len":N}`
    /// followed by N raw bytes on the same connection.
    pub async fn export_state(&mut self) -> io::Result<Vec<u8>> {
        self.send_command(r#"{"command":"export-state"}"#).await?;
        let resp = self.read_response().await?;
        if !resp.is_ok() {
            return Err(io::Error::other(resp.err_msg()));
        }
        let blob_len = resp
            .blob_len
            .ok_or_else(|| io::Error::other("export-state reply missing blob_len"))?
            as usize;
        let mut blob = vec![0u8; blob_len];
        self.reader.read_exact(&mut blob).await?;
        Ok(blob)
    }

    /// Import the single combined state blob into a listen-mode
    /// bhyve. The command carries `blob_len`, then the N raw bytes
    /// follow on the connection; bhyve reads its own destination
    /// VMM_TIME live and applies the cross-host adjustment. After
    /// this returns the caller sends [`Self::resume`] to start vCPU
    /// forward progress.
    pub async fn import_state(&mut self, blob: &[u8]) -> io::Result<()> {
        let cmd = format!(
            "{{\"command\":\"import-state\",\"blob_len\":{}}}",
            blob.len(),
        );
        self.send_command(&cmd).await?;
        self.writer.write_all(blob).await?;
        self.writer.flush().await?;
        let resp = self.read_response().await?;
        if resp.is_ok() {
            Ok(())
        } else {
            Err(io::Error::other(resp.err_msg()))
        }
    }

    /// Send a command and check the reply is `{"status":"ok"}`.
    /// Helper for the side-effect-only commands.
    async fn simple_cmd(&mut self, cmd: &str) -> io::Result<()> {
        self.send_command(cmd).await?;
        let resp = self.read_response().await?;
        if resp.is_ok() {
            Ok(())
        } else {
            Err(io::Error::other(resp.err_msg()))
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
