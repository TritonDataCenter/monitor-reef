// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Builds the status payload cn-agent POSTs to CNAPI.
//!
//! The legacy `StatusReporter` collects five pieces of data:
//! 1. VMs (vmadm lookup)
//! 2. Zpool info (bytes used/available per pool)
//! 3. Memory info (from kstat)
//! 4. Disk usage (deep zfs get + imgadm calls)
//! 5. System boot time
//!
//! The Rust port ships with (1) and (2) today, plus a timestamp. Memory and
//! disk usage rely on kstat and imgadm which have no portable FFI story in
//! Rust; they're tracked for follow-up ports. A missing field simply isn't
//! emitted in the JSON, matching the legacy "warn and skip" behavior.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;

use crate::smartos::{VmadmTool, ZfsTool};

/// Fields the legacy reporter requests from vmadm for each VM.
pub const VM_LOOKUP_FIELDS: &[&str] = &[
    "brand",
    "cpu_cap",
    "last_modified",
    "max_physical_memory",
    "owner_uuid",
    "quota",
    "state",
    "uuid",
    "zone_state",
];

/// One iteration's worth of data, ready to serialize to CNAPI.
///
/// Held as a `serde_json::Map` so partial data (when a collection step
/// fails) serializes as "this field just isn't there" rather than JSON
/// `null`, matching the legacy reporter.
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
/// `VmadmTool` and `ZfsTool` are injected so tests can point them at mocks.
/// When the tools fail, the corresponding field is skipped and a warning is
/// logged — that's the same graceful-degradation contract the legacy agent
/// had, so a transient `zfs` hiccup doesn't drop an entire heartbeat.
#[derive(Clone)]
pub struct StatusCollector {
    vmadm: Arc<VmadmTool>,
    zfs: Arc<ZfsTool>,
}

impl StatusCollector {
    pub fn new(vmadm: Arc<VmadmTool>, zfs: Arc<ZfsTool>) -> Self {
        Self { vmadm, zfs }
    }

    pub async fn collect(&self) -> StatusReport {
        let mut report = StatusReport::new();

        match self.collect_vms().await {
            Ok(vms) => {
                report.fields.insert("vms".to_string(), vms);
            }
            Err(e) => tracing::warn!(error = %e, "failed to collect VM status"),
        }

        match self.collect_zpools().await {
            Ok(zpools) => {
                report.fields.insert("zpoolStatus".to_string(), zpools);
            }
            Err(e) => tracing::warn!(error = %e, "failed to collect zpool status"),
        }

        report.fields.insert(
            "timestamp".to_string(),
            serde_json::Value::String(Utc::now().to_rfc3339()),
        );

        report
    }

    async fn collect_vms(&self) -> Result<serde_json::Value, String> {
        let search = BTreeMap::new();
        let opts = crate::smartos::vmadm::LookupOptions {
            include_dni: false,
            fields: Some(VM_LOOKUP_FIELDS.iter().map(|s| s.to_string()).collect()),
        };

        let vms = self
            .vmadm
            .lookup(&search, &opts)
            .await
            .map_err(|e| e.to_string())?;

        // Keyed by UUID so CNAPI can diff old vs new state.
        let mut by_uuid = serde_json::Map::new();
        for vm in vms {
            if let Some(uuid) = vm.get("uuid").and_then(|v| v.as_str()) {
                by_uuid.insert(uuid.to_string(), vm);
            }
        }
        Ok(serde_json::Value::Object(by_uuid))
    }

    async fn collect_zpools(&self) -> Result<serde_json::Value, String> {
        let pools = self.zfs.list_pools().await.map_err(|e| e.to_string())?;

        // Legacy shape: { <pool_name>: { bytes_available, bytes_used } }.
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
}
