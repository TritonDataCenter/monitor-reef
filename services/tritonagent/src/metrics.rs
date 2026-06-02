// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-CN metrics ticker.
//!
//! Every `interval` the ticker samples a fixed set of host kstats
//! (CPU, memory, VFS I/O, network, load average, established TCP),
//! turns each into [`tritond_metrics::Sample`]s under the matching
//! schema, and POSTs one [`tritond_metrics::SampleBatch`] to
//! tritond's `/v1/agent/metrics` endpoint.
//!
//! Two scopes per metric: `*_per_zone` (one VM, carries `instance_id`)
//! and `*_per_cn` (the global zone / whole-host view, no
//! `instance_id`). Per-zone samples need the kstat instance number
//! (= zoneid) resolved to the full zone UUID via `zoneadm list -p`,
//! because several kstat modules (`zones`, `memory_cap`, `zone_vfs`)
//! truncate the zonename to 30 bytes.
//!
//! Failures are best-effort: a transient kstat/zoneadm/dladm hiccup
//! or a 5xx from tritond logs a warning and the next tick retries.
//! The agent never buffers samples across ticks -- the metrics path
//! is intentionally lossy so a saturated control plane can shed load.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use tritond_client::Client;
use tritond_cn_platform::smartos::kstat::{ArcStats, LinkStat, ZoneCpu, ZoneDisk, ZoneMem};
use tritond_cn_platform::smartos::zfs::PoolIostatLatency;
use tritond_cn_platform::smartos::{KstatTool, ZfsTool};
use tritond_metrics::{
    Datum, Sample, SampleBatch, SampleIdentity, schema::cpu_mode, schemas, series,
};
use uuid::Uuid;

/// Default cadence -- matches the V5 dashboard's auto-refresh hint
/// (15s) so the freshest sample is at most one tick old when the
/// admin UI reloads.
pub const DEFAULT_METRICS_INTERVAL: Duration = Duration::from_secs(15);

/// Spawn the metrics ticker. Returns a [`MetricsHandle`] callers can
/// `shutdown().await` to drain the in-flight tick before exit.
pub fn spawn(
    client: Arc<Client>,
    cn_uuid: Uuid,
    kstat: Arc<KstatTool>,
    interval: Duration,
) -> MetricsHandle {
    let (tx, rx) = oneshot::channel::<()>();
    let join = tokio::spawn(run_loop(client, cn_uuid, kstat, interval, rx));
    MetricsHandle {
        join: Some(join),
        shutdown: Some(tx),
    }
}

/// JoinHandle + shutdown signal pair. Drop-safe: the tokio task ends
/// when the signal is sent, or when its sender is dropped.
pub struct MetricsHandle {
    join: Option<JoinHandle<()>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl MetricsHandle {
    /// Cleanly stop the ticker. Sends the shutdown signal, then awaits
    /// the join handle so the in-flight tick (if any) completes
    /// before this returns.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join.take()
            && let Err(e) = handle.await
        {
            warn!(error = %e, "metrics ticker join failed");
        }
    }
}

async fn run_loop(
    client: Arc<Client>,
    cn_uuid: Uuid,
    kstat: Arc<KstatTool>,
    interval: Duration,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(interval);
    // Skip the immediate first tick that tokio::interval fires by
    // default -- otherwise we hammer kstat as soon as the agent
    // boots, before tritond has finished bringing up its receivers.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = tick_once(&client, cn_uuid, &kstat).await {
                    warn!(error = %e, "metrics tick failed");
                }
            }
            _ = &mut shutdown => {
                debug!("metrics ticker shutdown");
                return;
            }
        }
    }
}

async fn tick_once(client: &Client, cn_uuid: Uuid, kstat: &KstatTool) -> anyhow::Result<()> {
    let now = Utc::now();

    // zoneid -> full zonename, for resolving the 30-byte-truncated
    // kstat name fields back to real UUIDs.
    let zone_names = zoneadm_zone_names().await.unwrap_or_else(|e| {
        warn!(error = %e, "zoneadm list failed; per-zone metrics may land per-CN this tick");
        HashMap::new()
    });
    // Physical NIC names, for splitting `link` kstats into per-CN vs
    // per-zone. Best-effort: an empty set just means no per-CN net
    // this tick.
    let phys = dladm_phys_links().await.unwrap_or_else(|e| {
        warn!(error = %e, "dladm show-phys failed; per-CN network skipped this tick");
        Vec::new()
    });

    let mut samples: Vec<Sample> = Vec::new();

    // --- CPU ---
    match kstat.cpu_per_zone().await {
        Ok(cpu) => push_cpu(&mut samples, cn_uuid, now, &zone_names, &cpu),
        Err(e) => warn!(error = %e, "kstat cpu_per_zone failed"),
    }
    // --- Memory: per-zone (memory_cap) + per-CN (system_pages + arc) ---
    match kstat.mem_per_zone().await {
        Ok(mem) => push_mem_per_zone(&mut samples, cn_uuid, now, &zone_names, &mem),
        Err(e) => warn!(error = %e, "kstat mem_per_zone failed"),
    }
    match kstat.memory_info().await {
        Ok(mi) => {
            let used = mi.total_bytes.saturating_sub(mi.availrmem_bytes);
            push_gauge_u64(
                &mut samples,
                schemas::MEM_PER_CN,
                cn_uuid,
                None,
                now,
                &[
                    (series::USED, used),
                    (series::ARC, mi.arcsize_bytes),
                    (series::TOTAL, mi.total_bytes),
                ],
            );
        }
        Err(e) => warn!(error = %e, "kstat memory_info failed"),
    }
    // --- Disk (VFS) bytes: zone 0 -> per-CN, others -> per-zone ---
    match kstat.disk_stats().await {
        Ok(disk) => push_disk(&mut samples, cn_uuid, now, &zone_names, &disk),
        Err(e) => warn!(error = %e, "kstat disk_stats failed"),
    }
    // --- Network bytes from `link` kstats ---
    match kstat.net_links().await {
        Ok(links) => push_net(&mut samples, cn_uuid, now, &zone_names, &phys, &links),
        Err(e) => warn!(error = %e, "kstat net_links failed"),
    }
    // --- Load average (per-CN only) ---
    match kstat.load_avg().await {
        Ok(Some(la)) => push_gauge_f64(
            &mut samples,
            schemas::LOAD_PER_CN,
            cn_uuid,
            None,
            now,
            &[
                (series::LOAD_1M, la.one),
                (series::LOAD_5M, la.five),
                (series::LOAD_15M, la.fifteen),
            ],
        ),
        Ok(None) => {}
        Err(e) => warn!(error = %e, "kstat load_avg failed"),
    }
    // --- Established TCP: netstack 0 -> per-CN, others -> per-zone ---
    match kstat.tcp_estab().await {
        Ok(tcp) => {
            for (zid, n) in tcp {
                if zid == 0 {
                    push_gauge_u64(
                        &mut samples,
                        schemas::SOCKETS_PER_CN,
                        cn_uuid,
                        None,
                        now,
                        &[(series::TCP_ESTAB, n)],
                    );
                } else if let Some(uuid) = resolve_zone_uuid(&zone_names, zid) {
                    push_gauge_u64(
                        &mut samples,
                        schemas::SOCKETS_PER_ZONE,
                        cn_uuid,
                        Some(uuid),
                        now,
                        &[(series::TCP_ESTAB, n)],
                    );
                }
            }
        }
        Err(e) => warn!(error = %e, "kstat tcp_estab failed"),
    }

    // --- Storage: ARC effectiveness (per-CN) + per-pool iostat ---
    // Time-series into ClickHouse so the Storage tab's Performance +
    // Cache·ARC views render trends without streaming `zpool iostat`
    // on the read path. Per-disk iostat + busy land in B2 alongside the
    // diskinfo/SMART device mapping.
    match kstat.arcstats().await {
        Ok(arc) => push_arc(&mut samples, cn_uuid, now, &arc),
        Err(e) => warn!(error = %e, "kstat arcstats failed"),
    }
    let zfs = ZfsTool::default();
    match zfs.list_pools().await {
        Ok(pools) => {
            for row in &pools {
                let Some(pool) = row.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                match zfs.pool_iostat_latency(pool).await {
                    Ok(io) => push_pool_iostat(&mut samples, cn_uuid, now, pool, &io),
                    Err(e) => warn!(error = %e, pool, "zpool iostat -l failed"),
                }
            }
        }
        Err(e) => warn!(error = %e, "zpool list failed"),
    }

    if samples.is_empty() {
        return Ok(());
    }
    debug!(count = samples.len(), "posting metrics batch");
    client
        .agent_metrics_ingest()
        .body(SampleBatch { samples })
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("agent_metrics_ingest: {e}"))?;
    Ok(())
}

// ---- per-metric sample builders ----------------------------------

fn push_cpu(
    out: &mut Vec<Sample>,
    cn_uuid: Uuid,
    now: DateTime<Utc>,
    zone_names: &HashMap<u32, String>,
    cpu: &[ZoneCpu],
) {
    for zc in cpu {
        let full = zone_names
            .get(&zc.zone_id)
            .map(String::as_str)
            .unwrap_or(zc.zone_name.as_str());
        let (instance_id, schema) = match Uuid::parse_str(full) {
            Ok(u) => (Some(u), schemas::CPU_PER_ZONE),
            Err(_) => (None, schemas::CPU_PER_CN),
        };
        for (mode, ns) in [
            (cpu_mode::USER, zc.user_ns),
            (cpu_mode::SYSTEM, zc.system_ns),
            (cpu_mode::IOWAIT, zc.iowait_ns),
        ] {
            out.push(Sample {
                schema: schema.into(),
                identity: ident(cn_uuid, instance_id, mode),
                timestamp: now,
                datum: Datum::CumulativeU64 { value: ns },
            });
        }
    }
}

fn push_mem_per_zone(
    out: &mut Vec<Sample>,
    cn_uuid: Uuid,
    now: DateTime<Utc>,
    zone_names: &HashMap<u32, String>,
    mem: &[ZoneMem],
) {
    for zm in mem {
        // The GZ's per-CN memory comes from `memory_info()` instead;
        // skip zone 0 here.
        if zm.zone_id == 0 {
            continue;
        }
        let Some(uuid) = resolve_zone_uuid_or_kstat(zone_names, zm.zone_id, &zm.zone_name) else {
            continue;
        };
        for (s, v) in [(series::RSS, zm.rss_bytes), (series::SWAP, zm.swap_bytes)] {
            out.push(Sample {
                schema: schemas::MEM_PER_ZONE.into(),
                identity: ident(cn_uuid, Some(uuid), s),
                timestamp: now,
                datum: Datum::GaugeU64 { value: v },
            });
        }
    }
}

fn push_disk(
    out: &mut Vec<Sample>,
    cn_uuid: Uuid,
    now: DateTime<Utc>,
    zone_names: &HashMap<u32, String>,
    disk: &[ZoneDisk],
) {
    for zd in disk {
        let (instance_id, schema) = if zd.zone_id == 0 {
            (None, schemas::DISK_PER_CN)
        } else {
            match resolve_zone_uuid_or_kstat(zone_names, zd.zone_id, &zd.zone_name) {
                Some(u) => (Some(u), schemas::DISK_PER_ZONE),
                None => continue,
            }
        };
        for (s, v) in [
            (series::READ_BYTES, zd.read_bytes),
            (series::WRITE_BYTES, zd.write_bytes),
        ] {
            out.push(Sample {
                schema: schema.into(),
                identity: ident(cn_uuid, instance_id, s),
                timestamp: now,
                datum: Datum::CumulativeU64 { value: v },
            });
        }
    }
}

fn push_net(
    out: &mut Vec<Sample>,
    cn_uuid: Uuid,
    now: DateTime<Utc>,
    zone_names: &HashMap<u32, String>,
    phys: &[String],
    links: &[LinkStat],
) {
    // Aggregate per-zone vnics (`z<zoneid>_*`) by zoneid, and phys
    // NICs into one per-CN bucket. Everything else (lo0, proteus*,
    // unattached vnics) is ignored.
    let mut per_zone: HashMap<u32, (u64, u64)> = HashMap::new();
    let mut cn_rx = 0u64;
    let mut cn_tx = 0u64;
    let mut saw_phys = false;
    for ls in links {
        if let Some(zid) = parse_zone_vnic(&ls.link) {
            let e = per_zone.entry(zid).or_insert((0, 0));
            e.0 = e.0.saturating_add(ls.rx_bytes);
            e.1 = e.1.saturating_add(ls.tx_bytes);
        } else if phys.iter().any(|p| p == &ls.link) {
            cn_rx = cn_rx.saturating_add(ls.rx_bytes);
            cn_tx = cn_tx.saturating_add(ls.tx_bytes);
            saw_phys = true;
        }
    }
    if saw_phys {
        push_cumulative_u64(
            out,
            schemas::NET_PER_CN,
            cn_uuid,
            None,
            now,
            &[(series::RX_BYTES, cn_rx), (series::TX_BYTES, cn_tx)],
        );
    }
    for (zid, (rx, tx)) in per_zone {
        if let Some(uuid) = resolve_zone_uuid(zone_names, zid) {
            push_cumulative_u64(
                out,
                schemas::NET_PER_ZONE,
                cn_uuid,
                Some(uuid),
                now,
                &[(series::RX_BYTES, rx), (series::TX_BYTES, tx)],
            );
        }
    }
}

/// ARC effectiveness counters (`hits`/`misses`/`l2_*`) +
/// sizing/composition gauges. Per-CN, no `device`.
fn push_arc(out: &mut Vec<Sample>, cn_uuid: Uuid, now: DateTime<Utc>, arc: &ArcStats) {
    for (s, v) in [
        (series::ARC_HITS, arc.hits),
        (series::ARC_MISSES, arc.misses),
        (series::ARC_L2_HITS, arc.l2_hits),
        (series::ARC_L2_MISSES, arc.l2_misses),
    ] {
        out.push(Sample {
            schema: schemas::ZFS_ARC_PER_CN.into(),
            identity: ident(cn_uuid, None, s),
            timestamp: now,
            datum: Datum::CumulativeU64 { value: v },
        });
    }
    for (s, v) in [
        (series::ARC_SIZE, arc.size),
        (series::ARC_TARGET, arc.target),
        (series::ARC_C_MAX, arc.c_max),
        (series::ARC_MFU, arc.mfu_size),
        (series::ARC_MRU, arc.mru_size),
        (series::ARC_METADATA, arc.metadata_size),
        (series::ARC_L2_SIZE, arc.l2_size),
    ] {
        out.push(Sample {
            schema: schemas::ZFS_ARC_SIZE_PER_CN.into(),
            identity: ident(cn_uuid, None, s),
            timestamp: now,
            datum: Datum::GaugeU64 { value: v },
        });
    }
}

/// Per-pool iostat: ops/bytes counters + end-to-end latency gauges.
/// `device` = pool name.
fn push_pool_iostat(
    out: &mut Vec<Sample>,
    cn_uuid: Uuid,
    now: DateTime<Utc>,
    pool: &str,
    io: &PoolIostatLatency,
) {
    for (s, v) in [
        (series::READ_OPS, io.read_ops),
        (series::WRITE_OPS, io.write_ops),
        (series::READ_BYTES, io.read_bw),
        (series::WRITE_BYTES, io.write_bw),
    ] {
        if let Some(v) = v {
            out.push(Sample {
                schema: schemas::DISK_IOSTAT_PER_CN.into(),
                identity: ident_device(cn_uuid, pool, s),
                timestamp: now,
                datum: Datum::CumulativeU64 { value: v },
            });
        }
    }
    for (s, v) in [
        (series::READ_LAT, io.read_lat_ns),
        (series::WRITE_LAT, io.write_lat_ns),
    ] {
        if let Some(v) = v {
            out.push(Sample {
                schema: schemas::DISK_LATENCY_PER_CN.into(),
                identity: ident_device(cn_uuid, pool, s),
                timestamp: now,
                datum: Datum::GaugeU64 { value: v },
            });
        }
    }
}

// ---- small helpers -----------------------------------------------

fn ident(cn_uuid: Uuid, instance_id: Option<Uuid>, mode: &str) -> SampleIdentity {
    SampleIdentity {
        cn_id: cn_uuid,
        tenant_id: None,
        project_id: None,
        instance_id,
        series: Some(mode.to_string()),
        device: None,
    }
}

/// Identity for a per-device/per-pool storage sample: no instance, a
/// `device` label (e.g. `c1t2d0` or a pool name) plus the sub-metric
/// `series` (e.g. `read_lat`).
fn ident_device(cn_uuid: Uuid, device: &str, series: &str) -> SampleIdentity {
    SampleIdentity {
        cn_id: cn_uuid,
        tenant_id: None,
        project_id: None,
        instance_id: None,
        series: Some(series.to_string()),
        device: Some(device.to_string()),
    }
}

fn push_gauge_u64(
    out: &mut Vec<Sample>,
    schema: &str,
    cn_uuid: Uuid,
    instance_id: Option<Uuid>,
    now: DateTime<Utc>,
    pairs: &[(&str, u64)],
) {
    for (s, v) in pairs {
        out.push(Sample {
            schema: schema.into(),
            identity: ident(cn_uuid, instance_id, s),
            timestamp: now,
            datum: Datum::GaugeU64 { value: *v },
        });
    }
}

fn push_gauge_f64(
    out: &mut Vec<Sample>,
    schema: &str,
    cn_uuid: Uuid,
    instance_id: Option<Uuid>,
    now: DateTime<Utc>,
    pairs: &[(&str, f64)],
) {
    for (s, v) in pairs {
        out.push(Sample {
            schema: schema.into(),
            identity: ident(cn_uuid, instance_id, s),
            timestamp: now,
            datum: Datum::GaugeF64 { value: *v },
        });
    }
}

fn push_cumulative_u64(
    out: &mut Vec<Sample>,
    schema: &str,
    cn_uuid: Uuid,
    instance_id: Option<Uuid>,
    now: DateTime<Utc>,
    pairs: &[(&str, u64)],
) {
    for (s, v) in pairs {
        out.push(Sample {
            schema: schema.into(),
            identity: ident(cn_uuid, instance_id, s),
            timestamp: now,
            datum: Datum::CumulativeU64 { value: *v },
        });
    }
}

/// Resolve a kstat instance (zoneid) to the full zone UUID, if the
/// zoneadm map knows it and the name parses as a UUID. `None` for the
/// GZ (zoneid 0 isn't in the map as a UUID), unknown zones, or
/// non-UUID zonenames.
fn resolve_zone_uuid(zone_names: &HashMap<u32, String>, zone_id: u32) -> Option<Uuid> {
    zone_names
        .get(&zone_id)
        .and_then(|n| Uuid::parse_str(n).ok())
}

/// Like [`resolve_zone_uuid`] but falls back to trying the (possibly
/// truncated) kstat name field if zoneadm didn't have the zone. The
/// truncated form won't parse as a UUID, so this just degrades to
/// `None` -- but it keeps the call sites symmetric.
fn resolve_zone_uuid_or_kstat(
    zone_names: &HashMap<u32, String>,
    zone_id: u32,
    kstat_name: &str,
) -> Option<Uuid> {
    resolve_zone_uuid(zone_names, zone_id).or_else(|| Uuid::parse_str(kstat_name).ok())
}

/// If `link` is a zone vnic named `z<zoneid>_net<N>`, return the
/// zoneid. Otherwise `None`.
fn parse_zone_vnic(link: &str) -> Option<u32> {
    let rest = link.strip_prefix('z')?;
    let (digits, tail) = rest.split_once('_')?;
    if !tail.starts_with("net") {
        return None;
    }
    digits.parse().ok()
}

/// Map running-zone IDs to their full zonenames via `zoneadm list -p`.
/// Output is one colon-separated line per zone:
/// `zoneid:zonename:state:zonepath:uuid:brand:ip-type:...`. We need
/// fields 0 (zoneid) and 1 (zonename); zone paths never contain `:`.
async fn zoneadm_zone_names() -> anyhow::Result<HashMap<u32, String>> {
    let output = tokio::process::Command::new("/usr/sbin/zoneadm")
        .args(["list", "-p"])
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "zoneadm list -p exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut f = line.split(':');
        let id_field = f.next().unwrap_or("");
        let name = f.next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if let Ok(zone_id) = id_field.parse::<u32>() {
            map.insert(zone_id, name.to_string());
        }
    }
    Ok(map)
}

/// Physical NIC datalink names via `dladm show-phys -p -o link`.
async fn dladm_phys_links() -> anyhow::Result<Vec<String>> {
    let output = tokio::process::Command::new("/usr/sbin/dladm")
        .args(["show-phys", "-p", "-o", "link"])
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "dladm show-phys exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zone_vnic_names() {
        assert_eq!(parse_zone_vnic("z12_net0"), Some(12));
        assert_eq!(parse_zone_vnic("z16_net1"), Some(16));
        assert_eq!(parse_zone_vnic("e1000g0"), None);
        assert_eq!(parse_zone_vnic("proteus1358398920"), None);
        assert_eq!(parse_zone_vnic("lo0"), None);
        assert_eq!(parse_zone_vnic("z12_vnic0"), None); // not net<N>
    }

    #[test]
    fn resolve_zone_uuid_only_for_known_uuid_named_zones() {
        let mut m = HashMap::new();
        m.insert(0u32, "global".to_string());
        m.insert(1u32, "a0f29ee3-0ec7-4e0c-9eca-f7332391c51d".to_string());
        m.insert(2u32, "some-non-uuid-zone".to_string());
        assert_eq!(resolve_zone_uuid(&m, 0), None);
        assert!(resolve_zone_uuid(&m, 1).is_some());
        assert_eq!(resolve_zone_uuid(&m, 2), None);
        assert_eq!(resolve_zone_uuid(&m, 99), None);
    }

    #[test]
    fn handle_shutdown_when_dropped() {
        let (tx, _rx) = oneshot::channel::<()>();
        let h = MetricsHandle {
            join: None,
            shutdown: Some(tx),
        };
        drop(h);
    }
}
