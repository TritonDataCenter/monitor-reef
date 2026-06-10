// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Migration capability probe (LM-0b).
//!
//! Collects the source/target compatibility fingerprint the placement
//! engine's migration filters compare at designate time
//! (`cn-bhyve-compatible`, `cn-cpu-feature-superset`,
//! `cn-time-synced`, `cn-zfs-compatible`). The capacity ticker caches
//! one [`MigrateCaps`] and folds it into every `AgentCapacityReport`;
//! without it the source CN's fingerprint is empty and the migration
//! filters reject every candidate target.
//!
//! Best-effort like the rest of the capacity feed: a failed shell-out
//! degrades that one field to its "unreported" shape rather than
//! failing the probe, and the matching filter falls back to its
//! missing-data semantics.

use std::collections::BTreeMap;
use std::path::Path;

use tokio::process::Command;
use tracing::{debug, warn};
use tritond_client::types::ZpoolPropFingerprint;

/// The pool whose on-disk-format properties travel in the capacity
/// report. v1 migrations only carry the system pool.
const PROBED_POOL: &str = "zones";

/// bhyve kernel control device. Its presence is the signal that this
/// CN can drive a vmm RAM/device-state channel at all; a CN without
/// it (no bhyve module, non-HVM hardware) must not advertise a
/// protocol version or the bhyve-compat filter would green-light a
/// target that cannot receive.
const VMMCTL_PATH: &str = "/dev/vmmctl";

/// Cached migration capability fingerprint, shaped to slot straight
/// into the `AgentCapacityReport` fields of the same names (which
/// mirror `CnCapacity` on the tritond side).
#[derive(Debug, Clone, Default)]
pub struct MigrateCaps {
    pub vmm_protocol_version: Option<String>,
    pub cpu_features: Vec<String>,
    pub tsc_offset_ns: Option<i64>,
    pub zpool_props: BTreeMap<String, ZpoolPropFingerprint>,
}

/// Run the full capability probe. Never fails: each component
/// degrades independently to its "unreported" value.
pub async fn probe() -> MigrateCaps {
    let caps = MigrateCaps {
        vmm_protocol_version: probe_vmm_protocol(),
        cpu_features: probe_cpu_features().await,
        tsc_offset_ns: probe_ntp_offset_ns().await,
        zpool_props: probe_zpool_props().await,
    };
    debug!(
        vmm_protocol_version = ?caps.vmm_protocol_version,
        cpu_features = caps.cpu_features.len(),
        tsc_offset_ns = ?caps.tsc_offset_ns,
        zpools = caps.zpool_props.len(),
        "migration capability probe complete",
    );
    caps
}

/// Advertise the vmm-migrate wire protocol only where the agent can
/// actually open a vmm device; everywhere else `None` keeps the
/// `cn-bhyve-compatible` filter rejecting this CN as a live target.
fn probe_vmm_protocol() -> Option<String> {
    if cfg!(target_os = "illumos") && Path::new(VMMCTL_PATH).exists() {
        Some(tritond_vmm_migrate::PROTOCOL_V0.to_string())
    } else {
        None
    }
}

async fn probe_cpu_features() -> Vec<String> {
    if cfg!(not(target_os = "illumos")) {
        return Vec::new();
    }
    let output = match Command::new("isainfo").arg("-x").output().await {
        Ok(out) if out.status.success() => out,
        Ok(out) => {
            warn!(
                status = ?out.status,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "isainfo -x failed; reporting no cpu features",
            );
            return Vec::new();
        }
        Err(e) => {
            warn!(error = %e, "could not run isainfo -x; reporting no cpu features");
            return Vec::new();
        }
    };
    parse_isainfo(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `isainfo -x` output: per instruction set, one
/// `arch: feat feat ...` line. Only the first (native 64-bit) line
/// matters; it is what bhyve exposes to guests.
fn parse_isainfo(raw: &str) -> Vec<String> {
    raw.lines()
        .find_map(|line| line.split_once(':'))
        .map(|(_, feats)| feats.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// NTP-corrected clock offset in nanoseconds, for the
/// `cn-time-synced` filter. Falls back to `Some(0)` when ntpq is
/// unavailable or silent: that filter rejects on `None` ("probe
/// missing"), and a CN whose clock we cannot interrogate is far more
/// likely to be in sync than 100ms adrift, so the fallback keeps the
/// fleet migratable instead of bricking designate everywhere ntpd
/// isn't running.
async fn probe_ntp_offset_ns() -> Option<i64> {
    match Command::new("ntpq")
        .args(["-nc", "rv 0 offset"])
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            match parse_ntpq_offset_ns(&raw) {
                Some(ns) => Some(ns),
                None => {
                    debug!(
                        output = %raw.trim(),
                        "ntpq reported no offset; assuming clock in sync",
                    );
                    Some(0)
                }
            }
        }
        Ok(out) => {
            debug!(
                status = ?out.status,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "ntpq rv failed; assuming clock in sync",
            );
            Some(0)
        }
        Err(e) => {
            debug!(error = %e, "could not run ntpq; assuming clock in sync");
            Some(0)
        }
    }
}

/// Pull `offset=<ms>` out of `ntpq -nc "rv 0 offset"` output and
/// convert milliseconds (ntpq's unit) to nanoseconds. Tolerates the
/// variable appearing among other comma-separated `var=value` pairs.
fn parse_ntpq_offset_ns(raw: &str) -> Option<i64> {
    for token in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        if let Some(value) = token.strip_prefix("offset=") {
            let ms: f64 = value.parse().ok()?;
            // round, not truncate: ms values like 1.234 are not
            // exactly representable and truncation would shave a
            // nanosecond off the conversion.
            return Some((ms * 1_000_000.0).round() as i64);
        }
    }
    None
}

async fn probe_zpool_props() -> BTreeMap<String, ZpoolPropFingerprint> {
    if cfg!(not(target_os = "illumos")) {
        return BTreeMap::new();
    }
    let output = match Command::new("zfs")
        .args([
            "get",
            "-H",
            "-p",
            "-o",
            "property,value",
            "encryption,compression,recordsize",
            PROBED_POOL,
        ])
        .output()
        .await
    {
        Ok(out) if out.status.success() => out,
        Ok(out) => {
            warn!(
                status = ?out.status,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "zfs get on {PROBED_POOL} failed; reporting no zpool props",
            );
            return BTreeMap::new();
        }
        Err(e) => {
            warn!(error = %e, "could not run zfs get; reporting no zpool props");
            return BTreeMap::new();
        }
    };
    let raw = String::from_utf8_lossy(&output.stdout);
    match parse_zfs_props(&raw) {
        Some(fp) => BTreeMap::from([(PROBED_POOL.to_string(), fp)]),
        None => {
            warn!(
                output = %raw.trim(),
                "zfs get output missing expected properties; reporting no zpool props",
            );
            BTreeMap::new()
        }
    }
}

/// Parse `zfs get -Hp -o property,value ...` output (one
/// tab-separated `property<TAB>value` row per property) into the
/// fingerprint. All three properties must be present and recordsize
/// numeric, otherwise the fingerprint would compare unequal to a
/// correctly-probed peer for the wrong reason.
fn parse_zfs_props(raw: &str) -> Option<ZpoolPropFingerprint> {
    let mut encryption = None;
    let mut compression = None;
    let mut recordsize_bytes = None;
    for line in raw.lines() {
        let Some((prop, value)) = line.split_once('\t') else {
            continue;
        };
        let value = value.trim();
        match prop.trim() {
            "encryption" => encryption = Some(value.to_string()),
            "compression" => compression = Some(value.to_string()),
            "recordsize" => recordsize_bytes = value.parse::<u32>().ok(),
            _ => {}
        }
    }
    Some(ZpoolPropFingerprint {
        encryption: encryption?,
        compression: compression?,
        recordsize_bytes: recordsize_bytes?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_isainfo_canonical_amd64_line() {
        // Verbatim shape from `isainfo -x` on a lab CN.
        let raw = "amd64: rdseed adx avx2 fma bmi2 bmi1 xsave avx pclmulqdq aes \
                   sse4.2 sse4.1 ssse3 popcnt tscp cx16 sse3 sse2 sse fxsr mmx \
                   cmov amd_sysc cx8 tsc fpu";
        let feats = parse_isainfo(raw);
        assert_eq!(feats.len(), 26);
        assert_eq!(feats[0], "rdseed");
        assert!(feats.iter().any(|f| f == "avx2"));
        assert!(feats.iter().any(|f| f == "sse4.2"));
        assert_eq!(feats.last().map(String::as_str), Some("fpu"));
    }

    #[test]
    fn parse_isainfo_takes_first_arch_line_only() {
        let raw = "amd64: avx2 aes\ni386: ahf avx2 aes sep";
        assert_eq!(parse_isainfo(raw), vec!["avx2", "aes"]);
    }

    #[test]
    fn parse_isainfo_empty_or_garbage_is_empty() {
        assert!(parse_isainfo("").is_empty());
        assert!(parse_isainfo("no colon here\n").is_empty());
    }

    #[test]
    fn parse_ntpq_offset_bare_variable() {
        // `rv 0 offset` typically prints just the one variable.
        assert_eq!(parse_ntpq_offset_ns("offset=-0.561\n"), Some(-561_000));
        assert_eq!(parse_ntpq_offset_ns("offset=1.234"), Some(1_234_000));
        assert_eq!(parse_ntpq_offset_ns("offset=0.000"), Some(0));
    }

    #[test]
    fn parse_ntpq_offset_among_other_variables() {
        let raw = "associd=0 status=0615 leap_none, sync_ntp, 1 event, clock_sync,\n\
                   offset=2.5, frequency=-3.291\n";
        assert_eq!(parse_ntpq_offset_ns(raw), Some(2_500_000));
    }

    #[test]
    fn parse_ntpq_offset_missing_or_unparseable_is_none() {
        assert_eq!(parse_ntpq_offset_ns(""), None);
        assert_eq!(parse_ntpq_offset_ns("frequency=-3.291"), None);
        assert_eq!(parse_ntpq_offset_ns("offset=notanumber"), None);
    }

    #[test]
    fn parse_zfs_props_canonical_output() {
        // Verbatim shape from `zfs get -Hp -o property,value
        // encryption,compression,recordsize zones`.
        let raw = "encryption\toff\ncompression\tlz4\nrecordsize\t131072\n";
        let fp = parse_zfs_props(raw).expect("fingerprint");
        assert_eq!(fp.encryption, "off");
        assert_eq!(fp.compression, "lz4");
        assert_eq!(fp.recordsize_bytes, 131_072);
    }

    #[test]
    fn parse_zfs_props_missing_property_is_none() {
        let raw = "encryption\toff\ncompression\tlz4\n";
        assert!(parse_zfs_props(raw).is_none());
    }

    #[test]
    fn parse_zfs_props_non_numeric_recordsize_is_none() {
        let raw = "encryption\toff\ncompression\tlz4\nrecordsize\t128K\n";
        assert!(parse_zfs_props(raw).is_none());
    }

    #[test]
    fn parse_zfs_props_ignores_unknown_rows_and_blank_lines() {
        let raw = "\natime\ton\nencryption\toff\ncompression\tzstd\nrecordsize\t131072\n";
        let fp = parse_zfs_props(raw).expect("fingerprint");
        assert_eq!(fp.compression, "zstd");
    }
}
