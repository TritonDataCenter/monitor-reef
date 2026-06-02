// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Schema names and dimension constants.
//!
//! Schema names are stable on the wire. Once tritond emits a schema
//! into ClickHouse, do not rename it -- write a new schema and version
//! it instead. (See oxide/oximeter's TOML versioning for the long-term
//! pattern; we keep things lighter here while only one schema ships.)

use serde::{Deserialize, Serialize};
use std::fmt;

/// Wrapper around a schema-name string. `String` newtype so call sites
/// can't accidentally mix free-form strings with schema identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct SchemaName(pub String);

impl SchemaName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SchemaName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SchemaName {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for SchemaName {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// String constants for the schemas this crate knows about.
///
/// Naming: `triton.<metric>_per_<scope>` where scope is `zone` (one
/// VM) or `cn` (the compute node's global zone / whole-host view).
/// `_per_zone` samples carry an `instance_id`; `_per_cn` do not.
pub mod schemas {
    /// Per-zone CPU usage. `series` = CPU mode (`user`/`system`/
    /// `iowait`). Datum `CumulativeU64` (nanoseconds since zone boot).
    pub const CPU_PER_ZONE: &str = "triton.cpu_per_zone";
    /// Per-CN CPU usage (the global zone). Same shape as
    /// `CPU_PER_ZONE` with no `instance_id`.
    pub const CPU_PER_CN: &str = "triton.cpu_per_cn";

    /// Per-zone memory. `series` = `rss` / `swap`. Datum `GaugeU64`
    /// (bytes). Source: `memory_cap:<zoneid>:<zone>:rss|swap`.
    pub const MEM_PER_ZONE: &str = "triton.mem_per_zone";
    /// Per-CN memory. `series` = `used` (total - availrmem) / `arc`
    /// (ZFS ARC) / `total`. Datum `GaugeU64` (bytes).
    pub const MEM_PER_CN: &str = "triton.mem_per_cn";

    /// Per-zone disk (VFS) throughput. `series` = `read_bytes` /
    /// `write_bytes`. Datum `CumulativeU64` (bytes). Source:
    /// `zone_vfs:<zoneid>:<zone>:nread|nwritten` (present only on
    /// kernels that track per-zone VFS stats; otherwise empty).
    pub const DISK_PER_ZONE: &str = "triton.disk_per_zone";
    /// Per-CN disk (VFS) throughput. Same shape; source
    /// `zone_vfs:0:global:nread|nwritten`.
    pub const DISK_PER_CN: &str = "triton.disk_per_cn";

    /// Per-zone network. `series` = `rx_bytes` / `tx_bytes`. Datum
    /// `CumulativeU64` (bytes). Source: `link:0:z<zoneid>_*:rbytes64|
    /// obytes64`, summed over the zone's vnics.
    pub const NET_PER_ZONE: &str = "triton.net_per_zone";
    /// Per-CN network. Same shape; source `link:0:<phys>:rbytes64|
    /// obytes64` summed over the host's physical NICs.
    pub const NET_PER_CN: &str = "triton.net_per_cn";

    /// Per-CN load average. `series` = `1m` / `5m` / `15m`. Datum
    /// `GaugeF64`. Source: `unix:0:system_misc:avenrun_*min / 256`.
    /// No per-zone equivalent (load average is a host concept).
    pub const LOAD_PER_CN: &str = "triton.load_per_cn";

    /// Per-zone established TCP connections. `series` = `tcp_estab`.
    /// Datum `GaugeU64`. Source: `tcp:<zoneid>:tcp:currEstab`.
    pub const SOCKETS_PER_ZONE: &str = "triton.sockets_per_zone";
    /// Per-CN established TCP connections (the global zone's
    /// netstack). Same shape; source `tcp:0:tcp:currEstab`.
    pub const SOCKETS_PER_CN: &str = "triton.sockets_per_cn";

    // ── Storage (CN-local ZFS) ──────────────────────────────────────
    // `device` identity field carries the disk (`c1t2d0`) or pool name;
    // `series` carries the sub-metric. A schema pins one datum type so
    // the read path can map it to a single ClickHouse table -- hence
    // counters and gauges of the same concept (e.g. ARC) split into two
    // schemas. Source: the disk I/O kstat class + `zfs:0:arcstats`.

    /// Per-disk I/O counters. `device` = disk, `series` = `read_ops` /
    /// `write_ops` / `read_bytes` / `write_bytes`. Datum `CumulativeU64`.
    pub const DISK_IOSTAT_PER_CN: &str = "triton.disk_iostat_per_cn";
    /// Per-disk service latency. `device` = disk, `series` = `read_lat` /
    /// `write_lat`. Datum `GaugeU64` (nanoseconds; `rlentime/reads`).
    pub const DISK_LATENCY_PER_CN: &str = "triton.disk_latency_per_cn";
    /// Per-disk busy fraction. `device` = disk, `series` = `busy`.
    /// Datum `GaugeF64` (0.0-1.0; `(rtime+wtime)` delta / interval).
    pub const DISK_BUSY_PER_CN: &str = "triton.disk_busy_per_cn";

    /// ARC effectiveness counters. `series` = `hits` / `misses` /
    /// `l2_hits` / `l2_misses`. Datum `CumulativeU64`. Source:
    /// `zfs:0:arcstats:{hits,misses,l2_hits,l2_misses}`.
    pub const ZFS_ARC_PER_CN: &str = "triton.zfs_arc_per_cn";
    /// ARC sizing gauges. `series` = `size` / `target` (c) / `c_max` /
    /// `mfu` / `mru` / `metadata` / `l2_size`. Datum `GaugeU64` (bytes).
    pub const ZFS_ARC_SIZE_PER_CN: &str = "triton.zfs_arc_size_per_cn";

    /// Per-pool capacity over time. `device` = pool, `series` = `alloc` /
    /// `free` / `size`. Datum `GaugeU64` (bytes). (Phase B3.)
    pub const ZPOOL_CAPACITY_PER_CN: &str = "triton.zpool_capacity_per_cn";
    /// Per-disk SMART temperature. `device` = disk, `series` = `temp_c`.
    /// Datum `GaugeU64` (degrees C). (Phase B2, with the SMART reader.)
    pub const DISK_TEMP_PER_CN: &str = "triton.disk_temp_per_cn";

    /// Every schema tritond knows how to ingest + serve, so a client can
    /// ask tritond "what can I query?" rather than reading source. Keep
    /// in sync with the consts above.
    pub const ALL: &[&str] = &[
        CPU_PER_ZONE,
        CPU_PER_CN,
        MEM_PER_ZONE,
        MEM_PER_CN,
        DISK_PER_ZONE,
        DISK_PER_CN,
        NET_PER_ZONE,
        NET_PER_CN,
        LOAD_PER_CN,
        SOCKETS_PER_ZONE,
        SOCKETS_PER_CN,
        DISK_IOSTAT_PER_CN,
        DISK_LATENCY_PER_CN,
        DISK_BUSY_PER_CN,
        ZFS_ARC_PER_CN,
        ZFS_ARC_SIZE_PER_CN,
        ZPOOL_CAPACITY_PER_CN,
        DISK_TEMP_PER_CN,
    ];
}

/// Dimension values for the `series` identity field on CPU schemas.
///
/// Mirrors the cmon agent's `cpu_user_usage` / `cpu_sys_usage` /
/// `cpu_wait_time` exposition for parity-of-intent with legacy
/// scrapers, just renamed to fit a single schema.
pub mod cpu_mode {
    pub const USER: &str = "user";
    pub const SYSTEM: &str = "system";
    pub const IOWAIT: &str = "iowait";

    /// All modes tritond emits for CPU schemas.
    pub const ALL: &[&str] = &[USER, SYSTEM, IOWAIT];
}

/// `series` values for the non-CPU schemas.
pub mod series {
    // mem_per_zone
    pub const RSS: &str = "rss";
    pub const SWAP: &str = "swap";
    // mem_per_cn
    pub const USED: &str = "used";
    pub const ARC: &str = "arc";
    pub const TOTAL: &str = "total";
    // disk_per_*
    pub const READ_BYTES: &str = "read_bytes";
    pub const WRITE_BYTES: &str = "write_bytes";
    // net_per_*
    pub const RX_BYTES: &str = "rx_bytes";
    pub const TX_BYTES: &str = "tx_bytes";
    // load_per_cn
    pub const LOAD_1M: &str = "1m";
    pub const LOAD_5M: &str = "5m";
    pub const LOAD_15M: &str = "15m";
    // sockets_per_*
    pub const TCP_ESTAB: &str = "tcp_estab";
    // disk_iostat_per_cn
    pub const READ_OPS: &str = "read_ops";
    pub const WRITE_OPS: &str = "write_ops";
    // (READ_BYTES / WRITE_BYTES reused from disk_per_* above)
    // disk_latency_per_cn
    pub const READ_LAT: &str = "read_lat";
    pub const WRITE_LAT: &str = "write_lat";
    // disk_busy_per_cn
    pub const BUSY: &str = "busy";
    // zfs_arc_per_cn (counters)
    pub const ARC_HITS: &str = "hits";
    pub const ARC_MISSES: &str = "misses";
    pub const ARC_L2_HITS: &str = "l2_hits";
    pub const ARC_L2_MISSES: &str = "l2_misses";
    // zfs_arc_size_per_cn (gauges)
    pub const ARC_SIZE: &str = "size";
    pub const ARC_TARGET: &str = "target";
    pub const ARC_C_MAX: &str = "c_max";
    pub const ARC_MFU: &str = "mfu";
    pub const ARC_MRU: &str = "mru";
    pub const ARC_METADATA: &str = "metadata";
    pub const ARC_L2_SIZE: &str = "l2_size";
    // zpool_capacity_per_cn
    pub const ALLOC: &str = "alloc";
    pub const FREE: &str = "free";
    pub const SIZE: &str = "size";
    // disk_temp_per_cn
    pub const TEMP_C: &str = "temp_c";
}
