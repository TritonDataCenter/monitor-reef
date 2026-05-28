// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Drive the bhyve memory reservoir via `/usr/lib/rsrvrctl`.
//!
//! The reservoir (RFD 0185) is a pool of physical memory the VMM driver
//! pre-reserves for bhyve guests. `rsrvrctl -q` reports its sizing and
//! `rsrvrctl -s <MiB>` resizes it toward a target. Sizes are MiB.
//!
//! Two properties of the underlying ioctl shape this wrapper:
//!
//! 1. `rsrvrctl` opens `/dev/vmmctl` with `O_EXCL`, so only one
//!    invocation can run at a time -- even a read-only `-q` fails while a
//!    resize holds the device. Every call therefore takes [`io_lock`],
//!    which serializes reservoir access within the process.
//! 2. A resize that runs out of free memory grows as far as it can and
//!    leaves the reservoir at that partial size (the kernel does not roll
//!    back), then `rsrvrctl` exits non-zero. [`set_target`] treats that as
//!    best-effort: it re-queries and returns the achieved size rather than
//!    erroring, so callers can compare achieved-vs-requested themselves.
//!
//! [`io_lock`]: ReservoirTool::io_lock

use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;
use tokio::sync::Mutex;

/// Path to the reservoir control tool on SmartOS compute nodes
/// (`system/bhyve` package: `usr/lib/rsrvrctl`).
pub const DEFAULT_RSRVRCTL_BIN: &str = "/usr/lib/rsrvrctl";

/// Resize step handed to `rsrvrctl -c`. Chunking keeps a large resize
/// incremental and responsive to signals rather than one long
/// uninterruptible ioctl.
pub const DEFAULT_CHUNK_MIB: u64 = 1024;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReservoirError {
    #[error("failed to spawn {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} exited with status {status}: {stderr}")]
    NonZeroExit {
        path: PathBuf,
        status: ExitStatus,
        stderr: String,
    },
    #[error("rsrvrctl query output missing field: {0}")]
    Missing(&'static str),
    #[error("rsrvrctl value for {field} is not a number: {raw}")]
    NotNumeric { field: &'static str, raw: String },
}

/// Reservoir sizing as reported by `rsrvrctl -q`. All values are MiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ReservoirState {
    /// Reserved but not yet handed to a guest.
    pub free_mib: u64,
    /// Reserved and allocated to (non-transient) guests.
    pub alloc_mib: u64,
    /// Memory bhyve guests took transiently because it was *not* drawn
    /// from the reservoir. Tracked separately; not part of the reservoir.
    pub transient_alloc_mib: u64,
    /// Kernel-enforced ceiling on total reservoir size for this host.
    pub limit_mib: u64,
}

impl ReservoirState {
    /// Current reservoir size (`free + alloc`) -- the quantity a
    /// `set_target` drives toward, matching the kernel's accounting.
    pub fn current_mib(&self) -> u64 {
        self.free_mib.saturating_add(self.alloc_mib)
    }
}

/// Wrapper around the `rsrvrctl` binary. Share a single instance (e.g.
/// behind an `Arc`) so the internal [`Mutex`] serializes all `/dev/vmmctl`
/// access across the status collector and the reservoir manager.
#[derive(Debug)]
pub struct ReservoirTool {
    pub bin: PathBuf,
    chunk_mib: u64,
    /// Serializes invocations -- `rsrvrctl` opens `/dev/vmmctl` `O_EXCL`.
    io_lock: Mutex<()>,
}

impl Default for ReservoirTool {
    fn default() -> Self {
        Self {
            bin: PathBuf::from(DEFAULT_RSRVRCTL_BIN),
            chunk_mib: DEFAULT_CHUNK_MIB,
            io_lock: Mutex::new(()),
        }
    }
}

impl ReservoirTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            ..Self::default()
        }
    }

    pub fn with_chunk_mib(mut self, chunk_mib: u64) -> Self {
        self.chunk_mib = chunk_mib.max(1);
        self
    }

    /// Query reservoir sizing, blocking on [`io_lock`] until any
    /// in-flight resize finishes.
    ///
    /// [`io_lock`]: ReservoirTool::io_lock
    pub async fn query(&self) -> Result<ReservoirState, ReservoirError> {
        let _guard = self.io_lock.lock().await;
        self.query_inner().await
    }

    /// Non-blocking query for hot paths (the heartbeat collector): returns
    /// `Ok(None)` if a resize currently holds the device rather than
    /// stalling the caller for the duration of the resize.
    pub async fn try_query(&self) -> Result<Option<ReservoirState>, ReservoirError> {
        match self.io_lock.try_lock() {
            Ok(_guard) => self.query_inner().await.map(Some),
            Err(_) => Ok(None),
        }
    }

    /// Resize the reservoir toward `target_mib`.
    ///
    /// Returns the achieved [`ReservoirState`]. If the resize cannot reach
    /// the target (insufficient free memory), the reservoir is left at the
    /// partial size the kernel managed and the achieved state is returned;
    /// the shortfall is logged, not raised as an error. Hard failures
    /// (cannot exec the tool) are returned as [`ReservoirError`].
    pub async fn set_target(&self, target_mib: u64) -> Result<ReservoirState, ReservoirError> {
        let _guard = self.io_lock.lock().await;

        let target = target_mib.to_string();
        let chunk = self.chunk_mib.to_string();
        let output = tokio::process::Command::new(&self.bin)
            .args(["-s", &target, "-c", &chunk])
            .output()
            .await
            .map_err(|source| ReservoirError::Spawn {
                path: self.bin.clone(),
                source,
            })?;

        if !output.status.success() {
            // Partial resize: the kernel keeps what it grew, so re-query
            // to learn the achieved size and treat the result as
            // best-effort rather than failing the caller.
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let achieved = self.query_inner().await?;
            tracing::warn!(
                target_mib,
                achieved_mib = achieved.current_mib(),
                stderr = stderr.trim(),
                "rsrvrctl -s did not reach target; using achieved size",
            );
            return Ok(achieved);
        }

        self.query_inner().await
    }

    /// Run `rsrvrctl -q` and parse it. Caller must hold [`io_lock`].
    ///
    /// [`io_lock`]: ReservoirTool::io_lock
    async fn query_inner(&self) -> Result<ReservoirState, ReservoirError> {
        let output = tokio::process::Command::new(&self.bin)
            .arg("-q")
            .output()
            .await
            .map_err(|source| ReservoirError::Spawn {
                path: self.bin.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(ReservoirError::NonZeroExit {
                path: self.bin.clone(),
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        parse_query(&String::from_utf8_lossy(&output.stdout))
    }
}

/// Parse `rsrvrctl -q` output. The format is one `Label:\t<MiB>` per line:
///
/// ```text
/// Free MiB:	0
/// Allocated MiB:	0
/// Transient Allocated MiB:	16480
/// Size limit MiB:	93639
/// ```
fn parse_query(text: &str) -> Result<ReservoirState, ReservoirError> {
    let mut free = None;
    let mut alloc = None;
    let mut transient = None;
    let mut limit = None;

    for line in text.lines() {
        let Some((label, raw)) = line.split_once('\t') else {
            continue;
        };
        let raw = raw.trim();
        let slot = match label.trim() {
            "Free MiB:" => (&mut free, "Free MiB"),
            "Allocated MiB:" => (&mut alloc, "Allocated MiB"),
            "Transient Allocated MiB:" => (&mut transient, "Transient Allocated MiB"),
            "Size limit MiB:" => (&mut limit, "Size limit MiB"),
            _ => continue,
        };
        let value = raw.parse::<u64>().map_err(|_| ReservoirError::NotNumeric {
            field: slot.1,
            raw: raw.to_string(),
        })?;
        *slot.0 = Some(value);
    }

    Ok(ReservoirState {
        free_mib: free.ok_or(ReservoirError::Missing("Free MiB"))?,
        alloc_mib: alloc.ok_or(ReservoirError::Missing("Allocated MiB"))?,
        transient_alloc_mib: transient.ok_or(ReservoirError::Missing("Transient Allocated MiB"))?,
        limit_mib: limit.ok_or(ReservoirError::Missing("Size limit MiB"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim output captured from `rsrvrctl -q` on a SmartOS CN.
    const SAMPLE: &str = "Free MiB:\t0
Allocated MiB:\t0
Transient Allocated MiB:\t16480
Size limit MiB:\t93639";

    #[test]
    fn parses_real_query_output() {
        let st = parse_query(SAMPLE).expect("parse");
        assert_eq!(st.free_mib, 0);
        assert_eq!(st.alloc_mib, 0);
        assert_eq!(st.transient_alloc_mib, 16_480);
        assert_eq!(st.limit_mib, 93_639);
        assert_eq!(st.current_mib(), 0);
    }

    #[test]
    fn current_is_free_plus_alloc() {
        let st = parse_query(
            "Free MiB:\t1024\nAllocated MiB:\t8192\nTransient Allocated MiB:\t0\nSize limit MiB:\t40000",
        )
        .expect("parse");
        assert_eq!(st.current_mib(), 9_216);
    }

    #[test]
    fn missing_field_is_an_error() {
        let err = parse_query("Free MiB:\t0\nAllocated MiB:\t0\nSize limit MiB:\t10").unwrap_err();
        assert!(matches!(
            err,
            ReservoirError::Missing("Transient Allocated MiB")
        ));
    }

    #[test]
    fn non_numeric_value_is_an_error() {
        let err = parse_query(
            "Free MiB:\tnope\nAllocated MiB:\t0\nTransient Allocated MiB:\t0\nSize limit MiB:\t10",
        )
        .unwrap_err();
        assert!(matches!(err, ReservoirError::NotNumeric { field: "Free MiB", .. }));
    }

    #[test]
    fn ignores_unrelated_lines() {
        let st = parse_query(
            "noise without tab\nFree MiB:\t5\nAllocated MiB:\t5\nTransient Allocated MiB:\t5\nSize limit MiB:\t50\ntrailing junk",
        )
        .expect("parse");
        assert_eq!(st.free_mib, 5);
        assert_eq!(st.limit_mib, 50);
    }
}
