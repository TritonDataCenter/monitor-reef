// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Neutral CN capacity sample: static hardware plus the live
//! instantaneous usage that forms the placement engine's
//! ClickHouse-independent floor (RFD 00005).
//!
//! This module stays free of any control-plane HTTP types — like the
//! rest of `cn_status`, the transport mapping (to the tritond client's
//! `AgentCapacityReport`) lives in the agent. The data sources are the
//! same ones the status collector already uses: `sysinfo` for CPU
//! topology / RAM total / platform, kstat for live free RAM and the
//! load average, and `zpool list` for per-pool size/free.

use crate::cn_status::collector::SysinfoLoader;
use crate::smartos::kstat::KstatTool;
use crate::smartos::zfs::ZfsTool;

/// A point-in-time capacity sample. Field shapes mirror the placement
/// engine's `cn-capacity` row but carry no tritond types.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapacitySample {
    pub cpu_cores_physical: u32,
    pub cpu_threads_logical: u32,
    pub ram_total_mb: u64,
    /// Live available RAM (MB) from kstat `availrmem` (free +
    /// reclaimable). `0` if kstat was unavailable this tick.
    pub ram_available_mb: u64,
    /// Live CPU utilisation (0.0 ..= 1.0): 1-minute load average over
    /// logical CPU count. `0.0` if unavailable.
    pub cpu_utilization_pct: f32,
    pub platform_version: String,
    pub hvm_supported: bool,
    pub zpools: Vec<ZpoolSample>,
}

/// Per-pool capacity. `tier` is a best-effort label
/// (`"ssd"`/`"nvme"`/`"hdd"`/`"mixed"`); v1 defaults to `"ssd"` pending
/// per-pool media classification (a later refinement).
#[derive(Debug, Clone, PartialEq)]
pub struct ZpoolSample {
    pub name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub tier: String,
}

/// Read a sysinfo number that may be encoded either as a JSON number
/// (`"CPU Total Cores": 40`) or a JSON string (`"MiB of Memory":
/// "65280"`) -- `/usr/bin/sysinfo` mixes both.
fn sysinfo_num(raw: &serde_json::Value, key: &str) -> Option<u64> {
    let v = raw.get(key)?;
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
}

/// Collect a capacity sample. Every sub-source is best-effort: a
/// missing tool (non-SmartOS dev host) or a transient kstat hiccup
/// leaves the matching fields at their defaults and logs a warning,
/// matching the status collector's degrade-don't-abort contract.
pub async fn collect_capacity(
    kstat: &KstatTool,
    zfs: &ZfsTool,
    sysinfo: &dyn SysinfoLoader,
) -> CapacitySample {
    let mut sample = CapacitySample::default();

    // CPU topology, RAM total, platform id (sysinfo).
    if let Some(si) = sysinfo.load().await {
        let raw = &si.raw;
        // "CPU Total Cores" is the logical/thread count on illumos;
        // "CPU Core Count" is the physical core count.
        sample.cpu_threads_logical = sysinfo_num(raw, "CPU Total Cores")
            .or_else(|| sysinfo_num(raw, "CPU Online Count"))
            .unwrap_or(0) as u32;
        sample.cpu_cores_physical = sysinfo_num(raw, "CPU Core Count")
            .unwrap_or(sample.cpu_threads_logical as u64) as u32;
        sample.ram_total_mb = sysinfo_num(raw, "MiB of Memory").unwrap_or(0);
        sample.platform_version = raw
            .get("Live Image")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    } else {
        tracing::warn!("capacity: sysinfo unavailable; CPU/RAM/platform left at defaults");
    }

    // The vnext compute fleet is bhyve; until a dedicated VMX/SVM probe
    // lands, advertise HVM so the cn-hvm-supported filter doesn't
    // reject every CN for bhyve placements.
    sample.hvm_supported = true;

    // Live RAM (kstat). `availrmem` already counts reclaimable pages,
    // so it is the "available right now" figure placement should trust.
    match kstat.memory_info().await {
        Ok(mi) => {
            sample.ram_available_mb = mi.availrmem_bytes / (1024 * 1024);
            if sample.ram_total_mb == 0 {
                sample.ram_total_mb = mi.total_bytes / (1024 * 1024);
            }
        }
        Err(e) => tracing::warn!(error = %e, "capacity: memory_info failed"),
    }

    // Live CPU (1-min load average normalised by logical CPU count).
    match kstat.load_avg().await {
        Ok(Some(load)) => {
            let threads = sample.cpu_threads_logical.max(1) as f64;
            sample.cpu_utilization_pct = (load.one / threads).clamp(0.0, 1.0) as f32;
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "capacity: load_avg failed"),
    }

    // Per-pool size/free (`zpool list -Hp`).
    match zfs.list_pools().await {
        Ok(pools) => {
            for row in pools {
                let Some(name) = row.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                let parse = |k: &str| {
                    row.get(k)
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0)
                };
                let alloc = parse("allocated");
                let free = parse("free");
                sample.zpools.push(ZpoolSample {
                    name: name.to_string(),
                    total_bytes: alloc.saturating_add(free),
                    free_bytes: free,
                    tier: "ssd".to_string(),
                });
            }
        }
        Err(e) => tracing::warn!(error = %e, "capacity: list_pools failed"),
    }

    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysinfo_num_reads_number_and_string() {
        let raw = serde_json::json!({
            "CPU Total Cores": 40,
            "MiB of Memory": "65280",
            "bad": "not-a-number",
        });
        assert_eq!(sysinfo_num(&raw, "CPU Total Cores"), Some(40));
        assert_eq!(sysinfo_num(&raw, "MiB of Memory"), Some(65280));
        assert_eq!(sysinfo_num(&raw, "bad"), None);
        assert_eq!(sysinfo_num(&raw, "missing"), None);
    }
}
