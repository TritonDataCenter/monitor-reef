// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-CN capacity ticker.
//!
//! Every `interval` the agent samples its structured capacity (static
//! hardware + live instantaneous usage) via `tritond-cn-platform` and
//! POSTs an `AgentCapacityReport` to tritond's `/v1/agent/capacity`.
//! This is the placement engine's capacity floor (RFD 00005): a CN
//! with no `cn-capacity` row is rejected by every placement filter, so
//! the agent keeps it fresh on its own cadence, independent of the
//! ClickHouse-derived load history.
//!
//! Best-effort and lossy, like the metrics ticker: a kstat/zpool hiccup
//! or a 5xx from tritond logs a warning and the next tick retries.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use tritond_client::Client;
use tritond_client::types::{
    AgentCapacityReport, NumaNode, StorageTier, UnderlayCapability, ZpoolCapacity,
};
use tritond_cn_platform::cn_status::{LiveSysinfo, collect_capacity};
use tritond_cn_platform::smartos::{KstatTool, ZfsTool};

use crate::migrate_probe::{self, MigrateCaps};

/// Default cadence. Live RAM/CPU for placement doesn't need the metrics
/// ticker's 15s granularity; 30s keeps the floor fresh without extra
/// control-plane chatter.
pub const DEFAULT_CAPACITY_INTERVAL: Duration = Duration::from_secs(30);

/// Migration capability probe refresh cadence. Protocol version, CPU
/// features and pool props only change with a PI or hardware swap;
/// the hourly re-probe exists to keep the NTP offset honest without
/// shelling out four commands on every 30s tick.
const MIGRATE_PROBE_REFRESH: Duration = Duration::from_secs(3600);

/// Spawn the capacity ticker. Returns a [`CapacityHandle`] callers can
/// `shutdown().await` to drain the in-flight tick before exit.
pub fn spawn(client: Arc<Client>, nic_tags: Vec<String>, interval: Duration) -> CapacityHandle {
    let (tx, rx) = oneshot::channel::<()>();
    let join = tokio::spawn(run_loop(client, nic_tags, interval, rx));
    CapacityHandle {
        join: Some(join),
        shutdown: Some(tx),
    }
}

/// JoinHandle + shutdown signal pair. Drop-safe: the task ends when the
/// signal is sent or its sender is dropped.
pub struct CapacityHandle {
    join: Option<JoinHandle<()>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl CapacityHandle {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join.take()
            && let Err(e) = handle.await
        {
            warn!(error = %e, "capacity ticker join failed");
        }
    }
}

async fn run_loop(
    client: Arc<Client>,
    nic_tags: Vec<String>,
    interval: Duration,
    mut shutdown: oneshot::Receiver<()>,
) {
    let kstat = KstatTool::new();
    let zfs = ZfsTool::new();
    let sysinfo = LiveSysinfo;
    // Probe once at startup so the very first report already carries
    // the migration fingerprint; the slow-moving result is cached and
    // refreshed on its own (much longer) cadence below.
    let mut migrate_caps = migrate_probe::probe().await;
    let mut probed_at = Instant::now();
    // The first `tick()` fires immediately so a freshly-bound CN
    // becomes placeable on the next tritond pick rather than after a
    // full interval.
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if probed_at.elapsed() >= MIGRATE_PROBE_REFRESH {
                    migrate_caps = migrate_probe::probe().await;
                    probed_at = Instant::now();
                }
                if let Err(e) =
                    tick_once(&client, &nic_tags, &kstat, &zfs, &sysinfo, &migrate_caps).await
                {
                    warn!(error = %e, "capacity tick failed");
                }
            }
            _ = &mut shutdown => {
                debug!("capacity ticker shutdown");
                return;
            }
        }
    }
}

async fn tick_once(
    client: &Client,
    nic_tags: &[String],
    kstat: &KstatTool,
    zfs: &ZfsTool,
    sysinfo: &LiveSysinfo,
    migrate_caps: &MigrateCaps,
) -> anyhow::Result<()> {
    let s = collect_capacity(kstat, zfs, sysinfo).await;
    let report = AgentCapacityReport {
        cpu_cores_physical: s.cpu_cores_physical,
        cpu_threads_logical: s.cpu_threads_logical,
        // Single UMA node until a per-node topology probe lands; the
        // placement engine treats a one-element list as UMA.
        numa_nodes: vec![NumaNode {
            node_id: 0,
            cores: s.cpu_cores_physical,
            ram_mb: s.ram_total_mb,
        }],
        ram_total_mb: s.ram_total_mb,
        ram_available_mb: s.ram_available_mb,
        cpu_utilization_pct: Some(s.cpu_utilization_pct),
        zpools: s
            .zpools
            .into_iter()
            .map(|z| ZpoolCapacity {
                name: z.name,
                total_bytes: z.total_bytes,
                free_bytes: z.free_bytes,
                tier: tier(&z.tier),
            })
            .collect(),
        nic_tags: nic_tags.to_vec(),
        underlay: UnderlayCapability {
            ipv4: true,
            ipv6: false,
        },
        // GPU / SR-IOV device inventory is a separate (future) probe;
        // left empty so the device filter Skips rather than misjudges.
        devices: vec![],
        platform_version: s.platform_version,
        hvm_supported: s.hvm_supported,
        cpu_features: migrate_caps.cpu_features.clone(),
        tsc_offset_ns: migrate_caps.tsc_offset_ns,
        vmm_protocol_version: migrate_caps.vmm_protocol_version.clone(),
        zpool_props: migrate_caps
            .zpool_props
            .iter()
            .map(|(pool, fp)| (pool.clone(), fp.clone()))
            .collect(),
    };
    client
        .agent_report_capacity()
        .body(report)
        .send()
        .await
        .context("agent_report_capacity")?;
    Ok(())
}

fn tier(s: &str) -> StorageTier {
    match s {
        "nvme" => StorageTier::Nvme,
        "hdd" => StorageTier::Hdd,
        "mixed" => StorageTier::Mixed,
        _ => StorageTier::Ssd,
    }
}
