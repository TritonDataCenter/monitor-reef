// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Builds the status payload the heartbeater hands to its [`StatusSink`].
//!
//! The legacy `StatusReporter` collects five pieces of data per tick:
//! 1. VMs (vmadm lookup with a fixed field list)
//! 2. Zpool info (bytes used/available per pool)
//! 3. Memory info (availrmem/arcsize/total, from kstat)
//! 4. Disk usage (per-VM + installed-image accounting via zfs)
//! 5. System boot time (from sysinfo)
//!
//! The Rust port reproduces every one of them. Individual sub-collectors
//! can fail without aborting the whole sample -- the legacy agent logged a
//! warning and skipped the field, and we preserve that contract so a
//! transient `kstat` hiccup doesn't cost us a heartbeat's worth of VM
//! state.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::cn_status::disk_usage::{DiskUsageSampler, VmSnapshot};
use crate::smartos::kstat::KstatTool;
use crate::smartos::reservoir::ReservoirTool;
use crate::smartos::sysinfo::Sysinfo;
use crate::smartos::{VmadmTool, ZfsTool};

/// Fields the legacy reporter requests from vmadm for each VM.
///
/// `zonepath` / `disks` are not in the legacy status payload itself, but the
/// disk-usage sampler needs them, so we fetch them in the same `vmadm
/// lookup` and strip them from the outbound copy.
///
/// `internal_metadata` and `nics` were added for the discovery + adoption
/// path: tritond's classifier reads the `tritond:*` identity keys out of
/// `internal_metadata` to distinguish managed-by-tritond zones from
/// pre-existing legacy zones, and uses `nics` for adoption pre-flight
/// (IP-collision check, network-rewrite preview).
pub const VM_LOOKUP_FIELDS: &[&str] = &[
    "alias",
    "brand",
    "cpu_cap",
    "disks",
    "do_not_inventory",
    "internal_metadata",
    "last_modified",
    "max_physical_memory",
    "nics",
    "owner_uuid",
    "quota",
    "state",
    "uuid",
    "zone_state",
    "zoneid",
    "zonename",
    "zonepath",
];

/// Subset of fields preserved in the vms map sent to the [`StatusSink`].
/// Mirrors the `newSample.vms[uuid] = { ... }` object in the legacy
/// heartbeater, plus `alias` (operator-friendly zone name, used as the
/// admin UI row name), `internal_metadata` (carries the `tritond:*`
/// identity keys for managed zones), and `nics` (full per-NIC layout
/// for adoption pre-flight + legacy NIC inventory).
pub const VM_POSTED_FIELDS: &[&str] = &[
    "alias",
    "brand",
    "cpu_cap",
    "internal_metadata",
    "last_modified",
    "max_physical_memory",
    "nics",
    "owner_uuid",
    "quota",
    "state",
    "uuid",
    "zone_state",
];

/// How many of the largest snapshots to retain in the heartbeat's snapshot
/// summary. Bounds payload size so per-VM image snapshots can't bloat
/// `last_status`.
const SNAPSHOT_TOP_N: usize = 25;

/// One iteration's worth of data, ready to serialize for the
/// [`crate::cn_status::StatusSink`].
#[derive(Debug, Clone, Default)]
pub struct StatusReport {
    pub fields: serde_json::Map<String, serde_json::Value>,
}

impl StatusReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_json(self) -> serde_json::Value {
        serde_json::Value::Object(self.fields)
    }
}

/// Collects a [`StatusReport`] from the system.
///
/// Dependencies are injected so tests can point each tool at a fixture or
/// mock. When a sub-collector fails, we log and skip the corresponding
/// field rather than aborting the sample.
#[derive(Clone)]
pub struct StatusCollector {
    vmadm: Arc<VmadmTool>,
    zfs: Arc<ZfsTool>,
    kstat: Arc<KstatTool>,
    reservoir: Arc<ReservoirTool>,
    disk_usage: DiskUsageSampler,
    sysinfo_loader: Arc<dyn SysinfoLoader>,
}

/// Abstraction over "give me the current sysinfo snapshot".
///
/// Production uses [`LiveSysinfo`] which runs `/usr/bin/sysinfo`. Tests use
/// a stub that returns a pre-loaded JSON value without spawning anything.
#[async_trait::async_trait]
pub trait SysinfoLoader: Send + Sync + 'static {
    async fn load(&self) -> Option<Sysinfo>;
}

/// Default sysinfo loader -- runs `/usr/bin/sysinfo` each call.
#[derive(Debug, Clone, Default)]
pub struct LiveSysinfo;

#[async_trait::async_trait]
impl SysinfoLoader for LiveSysinfo {
    async fn load(&self) -> Option<Sysinfo> {
        match Sysinfo::collect().await {
            Ok(si) => Some(si),
            Err(e) => {
                tracing::warn!(error = %e, "failed to read sysinfo");
                None
            }
        }
    }
}

impl StatusCollector {
    /// Production constructor wiring every default tool together.
    pub fn new(
        vmadm: Arc<VmadmTool>,
        zfs: Arc<ZfsTool>,
        kstat: Arc<KstatTool>,
        reservoir: Arc<ReservoirTool>,
        disk_usage: DiskUsageSampler,
    ) -> Self {
        Self {
            vmadm,
            zfs,
            kstat,
            reservoir,
            disk_usage,
            sysinfo_loader: Arc::new(LiveSysinfo),
        }
    }

    /// Swap in a non-default sysinfo loader (tests).
    pub fn with_sysinfo_loader(mut self, loader: Arc<dyn SysinfoLoader>) -> Self {
        self.sysinfo_loader = loader;
        self
    }

    /// Collect one status sample.
    pub async fn collect(&self) -> StatusReport {
        let mut report = StatusReport::new();

        // Step 1: vmadm lookup -- needed twice (outbound vms map and
        // disk-usage accounting), so run once and fan out.
        let vms_full = match self.lookup_vms().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "failed to collect VM list");
                Vec::new()
            }
        };
        report
            .fields
            .insert("vms".to_string(), vms_for_post(&vms_full));

        // Step 2: zpool info.
        match self.collect_zpools().await {
            Ok(z) => {
                report.fields.insert("zpoolStatus".to_string(), z);
            }
            Err(e) => tracing::warn!(error = %e, "failed to collect zpool status"),
        }

        // Step 2b: detailed zpool health + per-device iostat, surfaced by the
        // admin Storage tab (and, later, the issue evaluator). Best-effort: a
        // parse hiccup or a non-SmartOS dev host just skips the fields. Kept
        // separate from `zpoolStatus` above, whose shape downstream consumers
        // (tcadm, the classifier) depend on.
        match self.collect_zpool_detail().await {
            Ok((health, devices)) => {
                report.fields.insert("zpoolHealth".to_string(), health);
                report.fields.insert("zpoolDevices".to_string(), devices);
            }
            Err(e) => tracing::warn!(error = %e, "failed to collect zpool detail"),
        }

        // Step 2c: snapshot inventory summary (count + total + largest N).
        match self.zfs.snapshot_summary(SNAPSHOT_TOP_N).await {
            Ok(summary) => match serde_json::to_value(summary) {
                Ok(v) => {
                    report.fields.insert("zfsSnapshotSummary".to_string(), v);
                }
                Err(e) => tracing::warn!(error = %e, "failed to encode snapshot summary"),
            },
            Err(e) => tracing::warn!(error = %e, "failed to collect snapshot summary"),
        }

        // Step 3: memory info.
        match self.kstat.memory_info().await {
            Ok(mi) => match serde_json::to_value(mi) {
                Ok(v) => {
                    report.fields.insert("meminfo".to_string(), v);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to encode memory info");
                }
            },
            Err(e) => tracing::warn!(error = %e, "failed to collect memory info"),
        }

        // Step 3b: bhyve memory reservoir sizing. Best-effort and
        // non-blocking -- `try_query` returns `None` while a resize holds
        // `/dev/vmmctl`, and a missing `rsrvrctl` (non-SmartOS dev host)
        // just skips the field. Placement reads `limit_mib`/`alloc_mib`
        // from here to size reservoir-backed headroom.
        match self.reservoir.try_query().await {
            Ok(Some(rs)) => match serde_json::to_value(rs) {
                Ok(v) => {
                    report.fields.insert("reservoir".to_string(), v);
                }
                Err(e) => tracing::warn!(error = %e, "failed to encode reservoir state"),
            },
            Ok(None) => tracing::debug!("reservoir busy (resize in progress); skipping this tick"),
            Err(e) => tracing::debug!(error = %e, "failed to query reservoir"),
        }

        // Step 4: disk usage.
        let vm_snapshots = vm_snapshots_for_disk_usage(&vms_full);
        match self.disk_usage.sample(&vm_snapshots).await {
            Ok(du) => match serde_json::to_value(du) {
                Ok(v) => {
                    report.fields.insert("diskinfo".to_string(), v);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to encode disk usage");
                }
            },
            Err(e) => tracing::warn!(error = %e, "failed to collect disk usage"),
        }

        // Step 5: boot time from sysinfo (best-effort -- sysinfo can change
        // during the lifetime of the agent).
        if let Some(si) = self.sysinfo_loader.load().await
            && let Some(ts) = si.boot_time_unix()
            && let Some(iso) = unix_seconds_to_rfc3339(ts)
        {
            report
                .fields
                .insert("boot_time".to_string(), serde_json::Value::String(iso));
        }

        report.fields.insert(
            "timestamp".to_string(),
            serde_json::Value::String(Utc::now().to_rfc3339()),
        );

        report
    }

    async fn lookup_vms(&self) -> Result<Vec<serde_json::Value>, String> {
        let opts = crate::smartos::vmadm::LookupOptions {
            include_dni: false,
            fields: Some(VM_LOOKUP_FIELDS.iter().map(|s| s.to_string()).collect()),
        };
        self.vmadm
            .lookup(&BTreeMap::new(), &opts)
            .await
            .map_err(|e| e.to_string())
    }

    async fn collect_zpools(&self) -> Result<serde_json::Value, String> {
        let pools = self.zfs.list_pools().await.map_err(|e| e.to_string())?;

        let mut out = serde_json::Map::new();
        for row in pools {
            let name = row
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "zpool row missing 'name' field".to_string())?;
            let allocated = row
                .get("allocated")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let free = row
                .get("free")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            out.insert(
                name.to_string(),
                serde_json::json!({
                    "bytes_available": free,
                    "bytes_used": allocated,
                }),
            );
        }
        Ok(serde_json::Value::Object(out))
    }

    /// Detailed zpool health (`zpool status -v`) plus per-device iostat,
    /// each keyed by pool name. Returns `(zpoolHealth, zpoolDevices)`.
    async fn collect_zpool_detail(
        &self,
    ) -> Result<(serde_json::Value, serde_json::Value), String> {
        let pools = self.zfs.pool_status_all().await.map_err(|e| e.to_string())?;

        let mut health = serde_json::Map::new();
        let mut devices = serde_json::Map::new();
        for pool in &pools {
            let value = serde_json::to_value(pool).map_err(|e| e.to_string())?;
            health.insert(pool.name.clone(), value);

            match self.zfs.pool_iostat(&pool.name).await {
                Ok(rows) => match serde_json::to_value(rows) {
                    Ok(v) => {
                        devices.insert(pool.name.clone(), v);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, pool = %pool.name, "failed to encode iostat")
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, pool = %pool.name, "failed to collect iostat")
                }
            }
        }
        Ok((
            serde_json::Value::Object(health),
            serde_json::Value::Object(devices),
        ))
    }
}

/// Project the full vmadm lookup into the minimal map sent to the
/// [`crate::cn_status::StatusSink`].
///
/// Filters out DNI VMs (legacy behavior: they're not part of the
/// inventory) and strips fields the consumer doesn't retain.
fn vms_for_post(vms: &[serde_json::Value]) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for vm in vms {
        if vm
            .get("do_not_inventory")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let Some(uuid) = vm.get("uuid").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut projected = serde_json::Map::new();
        for field in VM_POSTED_FIELDS {
            if let Some(v) = vm.get(*field) {
                projected.insert((*field).to_string(), v.clone());
            }
        }
        out.insert(uuid.to_string(), serde_json::Value::Object(projected));
    }
    serde_json::Value::Object(out)
}

/// Extract the subset of fields the disk-usage sampler needs.
fn vm_snapshots_for_disk_usage(vms: &[serde_json::Value]) -> Vec<VmSnapshot> {
    let mut out = Vec::with_capacity(vms.len());
    for vm in vms {
        let Some(uuid_str) = vm.get("uuid").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(uuid) = Uuid::parse_str(uuid_str) else {
            continue;
        };
        let brand = vm.get("brand").and_then(|v| v.as_str()).map(str::to_string);
        let zonepath = vm
            .get("zonepath")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // disks is an array of {path, ...} objects. Legacy matches on
        // `device.path`; we pull the same field.
        let disks = vm
            .get("disks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| d.get("path").and_then(|p| p.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        out.push(VmSnapshot {
            uuid,
            brand,
            zonepath,
            disks,
        });
    }
    out
}

/// Convert unix seconds to an RFC 3339 / ISO 8601 string.
///
/// Matches the legacy formulation: `new Date(parseInt(sysinfo['Boot Time'],
/// 10) * 1000).toISOString()`. Returns `None` only if chrono rejects the
/// value (impossible for any reasonable boot-time input).
fn unix_seconds_to_rfc3339(seconds: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp(seconds, 0).map(|dt| dt.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vms_for_post_strips_dni_and_extra_fields() {
        let vms = vec![
            serde_json::json!({
                "uuid": "abc",
                "brand": "joyent",
                "state": "running",
                "zonepath": "/zones/abc",
                "do_not_inventory": false
            }),
            serde_json::json!({
                "uuid": "dni",
                "brand": "kvm",
                "do_not_inventory": true
            }),
        ];
        let out = vms_for_post(&vms);
        let map = out.as_object().expect("object");
        assert!(map.contains_key("abc"));
        assert!(!map.contains_key("dni"));
        // Only VM_POSTED_FIELDS survive: zonepath should have been dropped.
        assert!(map["abc"].get("zonepath").is_none());
        assert_eq!(map["abc"]["state"], "running");
    }

    #[test]
    fn vm_snapshots_pull_disks() {
        let vms = vec![serde_json::json!({
            "uuid": "aaaaaaaa-1111-2222-3333-444444444444",
            "brand": "kvm",
            "zonepath": "/zones/aaaaaaaa-1111-2222-3333-444444444444",
            "disks": [
                {"path": "/dev/zvol/rdsk/zones/aaaaaaaa-disk0", "size": 10240},
                {"path": "/dev/zvol/rdsk/zones/aaaaaaaa-disk1"}
            ]
        })];
        let snaps = vm_snapshots_for_disk_usage(&vms);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].disks.len(), 2);
        assert_eq!(snaps[0].brand.as_deref(), Some("kvm"));
    }

    #[test]
    fn unix_seconds_formats_as_rfc3339() {
        let got = unix_seconds_to_rfc3339(1_700_000_000).expect("rfc3339");
        // Just sanity-check: the year must be 2023, and the timezone must
        // be UTC ("+00:00").
        assert!(got.starts_with("2023-"), "got {got}");
        assert!(got.ends_with("+00:00"), "got {got}");
    }
}
