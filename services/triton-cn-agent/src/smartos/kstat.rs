// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Read selected kernel statistics via `/usr/bin/kstat`.
//!
//! The legacy cn-agent links against the `kstat` Node.js native add-on
//! (`bindings('kstat').Reader`) to pull memory accounting out of the
//! kernel. Rust has no production-ready libkstat crate today, so we shell
//! out to `kstat -p` instead — it accepts `module:instance:name:stat`
//! selectors and emits one stat per line, tab-separated, which is fast
//! enough to run every status-reporter tick.

use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;

/// Path to the kstat binary on SmartOS compute nodes.
pub const DEFAULT_KSTAT_BIN: &str = "/usr/bin/kstat";

/// Page size the legacy agent assumes (illumos x86 page size). All kstat
/// `*rmem`/`pagestotal` values are in pages; we multiply by this to get
/// bytes so the wire format matches the JS implementation verbatim.
pub const PAGE_SIZE_BYTES: u64 = 4096;

#[derive(Debug, Error)]
pub enum KstatError {
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
    #[error("kstat output missing expected stat: {0}")]
    Missing(&'static str),
    #[error("kstat value for {selector} is not a number: {raw}")]
    NotNumeric { selector: &'static str, raw: String },
}

/// Memory accounting matching the legacy SmartosBackend.getMemoryInfo shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct MemoryInfo {
    /// Free + reclaimable resident memory, in bytes.
    pub availrmem_bytes: u64,
    /// ZFS ARC size, in bytes.
    pub arcsize_bytes: u64,
    /// Total physical memory, in bytes.
    pub total_bytes: u64,
}

/// Wrapper around the `kstat` binary.
#[derive(Debug, Clone)]
pub struct KstatTool {
    pub bin: PathBuf,
}

impl Default for KstatTool {
    fn default() -> Self {
        Self {
            bin: PathBuf::from(DEFAULT_KSTAT_BIN),
        }
    }
}

impl KstatTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        Self { bin: bin.into() }
    }

    /// Return memory info in the shape the legacy agent produced.
    ///
    /// Runs `kstat -p` with three selectors in a single invocation rather
    /// than three separate calls; this keeps heartbeat overhead minimal.
    pub async fn memory_info(&self) -> Result<MemoryInfo, KstatError> {
        const AVAILRMEM: &str = "unix:0:system_pages:availrmem";
        const PAGESTOTAL: &str = "unix:0:system_pages:pagestotal";
        const ARCSIZE: &str = "zfs:0:arcstats:size";

        let output = tokio::process::Command::new(&self.bin)
            .args(["-p", AVAILRMEM, PAGESTOTAL, ARCSIZE])
            .output()
            .await
            .map_err(|source| KstatError::Spawn {
                path: self.bin.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(KstatError::NonZeroExit {
                path: self.bin.clone(),
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let availrmem = parse_stat(&text, AVAILRMEM, "availrmem")?;
        let pagestotal = parse_stat(&text, PAGESTOTAL, "pagestotal")?;
        let arcsize = parse_stat(&text, ARCSIZE, "arcstats:size")?;

        Ok(MemoryInfo {
            availrmem_bytes: availrmem.saturating_mul(PAGE_SIZE_BYTES),
            arcsize_bytes: arcsize,
            total_bytes: pagestotal.saturating_mul(PAGE_SIZE_BYTES),
        })
    }
}

/// Locate `selector` in tab-separated kstat output and parse its value.
fn parse_stat(text: &str, selector: &str, what: &'static str) -> Result<u64, KstatError> {
    for line in text.lines() {
        let mut parts = line.splitn(2, '\t');
        let key = parts.next().unwrap_or("");
        if key != selector {
            continue;
        }
        let raw = parts.next().ok_or(KstatError::Missing(what))?.trim();
        return raw.parse::<u64>().map_err(|_| KstatError::NotNumeric {
            selector: what,
            raw: raw.to_string(),
        });
    }
    Err(KstatError::Missing(what))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
unix:0:system_pages:availrmem\t1352259
unix:0:system_pages:pagestotal\t8312406
zfs:0:arcstats:size\t12412412552";

    #[test]
    fn parses_known_selectors() {
        assert_eq!(
            parse_stat(SAMPLE, "unix:0:system_pages:availrmem", "availrmem").expect("parse"),
            1_352_259
        );
        assert_eq!(
            parse_stat(SAMPLE, "zfs:0:arcstats:size", "arcstats:size").expect("parse"),
            12_412_412_552
        );
    }

    #[test]
    fn missing_selector_is_an_error() {
        let err = parse_stat(SAMPLE, "unix:0:system_pages:freemem", "freemem").unwrap_err();
        assert!(matches!(err, KstatError::Missing("freemem")));
    }

    #[test]
    fn non_numeric_value_is_an_error() {
        let bad = "unix:0:system_pages:availrmem\tnot_a_number";
        let err = parse_stat(bad, "unix:0:system_pages:availrmem", "availrmem").unwrap_err();
        assert!(matches!(err, KstatError::NotNumeric { .. }));
    }
}
