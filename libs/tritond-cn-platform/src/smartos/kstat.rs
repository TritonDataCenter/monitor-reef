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
//! out to `kstat -p` instead -- it accepts `module:instance:name:stat`
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
#[non_exhaustive]
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

/// Cumulative CPU usage for a single zone (or the global zone),
/// in nanoseconds. All three counters are monotonic since zone boot.
///
/// Mirrors the `zones:N:<zone_name>:nsec_{user,sys,waitrq}` kstats
/// the legacy cmon-agent reads. Consumers compute per-second deltas
/// at the storage layer; this struct just carries raw counter values.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ZoneCpu {
    /// Zone ID -- the kstat *instance* number, which the kernel sets
    /// equal to the running zone's zoneid. This is the reliable key:
    /// the kstat *name* field is the zonename truncated to 30 chars
    /// (so a 36-char VM UUID comes back mangled), but the zoneid
    /// round-trips cleanly against `zoneadm list -p`.
    pub zone_id: u32,
    /// Zone name as kstat reports it -- **truncated to 30 bytes** by
    /// the kernel. `"global"` for the GZ; a UUID *prefix* (not the
    /// full UUID) for Triton VM zones. Callers that need the real
    /// UUID should look it up by `zone_id` via `zoneadm list -p`.
    pub zone_name: String,
    pub user_ns: u64,
    pub system_ns: u64,
    pub iowait_ns: u64,
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

    /// Read `(nsec_user, nsec_sys, nsec_waitrq)` for every zone on the
    /// host. Issues one `kstat -p` invocation listing all three
    /// selectors so we don't pay the fork cost three times per
    /// metrics tick.
    ///
    /// Returns one [`ZoneCpu`] per kstat zone instance, keyed by
    /// zone name (= VM UUID for Triton VMs, `"global"` for the GZ).
    /// Zones missing any of the three stats are silently skipped --
    /// kstat occasionally races with zone teardown, and a metrics
    /// tick should not fail on a transient inconsistency.
    pub async fn cpu_per_zone(&self) -> Result<Vec<ZoneCpu>, KstatError> {
        const NSEC_USER: &str = "zones:::nsec_user";
        const NSEC_SYS: &str = "zones:::nsec_sys";
        const NSEC_WAITRQ: &str = "zones:::nsec_waitrq";

        let output = tokio::process::Command::new(&self.bin)
            .args(["-p", NSEC_USER, NSEC_SYS, NSEC_WAITRQ])
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
        Ok(parse_zone_cpu(&text))
    }
}

/// Per-zone memory snapshot from the `memory_cap` kstat module.
/// Sizes are bytes. `zone_id` is the kstat instance (== zoneid);
/// `zone_name` is the (truncated) kstat name field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ZoneMem {
    pub zone_id: u32,
    pub zone_name: String,
    pub rss_bytes: u64,
    pub swap_bytes: u64,
}

/// Per-zone VFS (filesystem) I/O from `zone_vfs`. Cumulative bytes.
/// Includes `zone_id == 0` (the global zone) when the kernel tracks
/// it. `reads`/`writes` op counts are intentionally dropped -- the
/// dashboard plots throughput, not IOPS.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ZoneDisk {
    pub zone_id: u32,
    pub zone_name: String,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// Per-datalink RX/TX bytes from the `link` kstat module. `link` is
/// the GZ-level datalink name -- `z<zoneid>_net<N>` for a zone vnic,
/// `<phys>` (e.g. `e1000g0`) for a physical NIC, plus `lo0`,
/// `proteus*`, etc. which the caller filters.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LinkStat {
    pub link: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// System load average (1/5/15 minute), already divided by the
/// kstat's 256x scaling factor.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct LoadAvg {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

/// ZFS ARC effectiveness + sizing, read from `zfs:0:arcstats`.
///
/// `hits`/`misses`/`l2_*` are monotonic counters (the storage layer
/// derives hit-ratio + miss-rate from per-bucket deltas); the `*_size`
/// fields are instantaneous byte gauges. Missing stats stay 0 -- field
/// names vary slightly across illumos vintages (`metadata_size` is
/// absent on some), and an absent stat should not fail the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct ArcStats {
    pub hits: u64,
    pub misses: u64,
    pub l2_hits: u64,
    pub l2_misses: u64,
    /// Current ARC size, bytes.
    pub size: u64,
    /// Target size `c`, bytes.
    pub target: u64,
    /// Max size `c_max`, bytes.
    pub c_max: u64,
    pub mfu_size: u64,
    pub mru_size: u64,
    pub metadata_size: u64,
    pub l2_size: u64,
}

impl KstatTool {
    /// Read per-zone RSS + swap from `memory_cap:::rss memory_cap:::swap`.
    /// Zones missing either stat are dropped.
    pub async fn mem_per_zone(&self) -> Result<Vec<ZoneMem>, KstatError> {
        let text = self
            .run_kstat(&["memory_cap:::rss", "memory_cap:::swap"])
            .await?;
        #[derive(Default)]
        struct Acc {
            zone_id: u32,
            name: String,
            rss: Option<u64>,
            swap: Option<u64>,
        }
        let mut acc: std::collections::HashMap<u32, Acc> = std::collections::HashMap::new();
        for (module, instance, name, stat, raw) in parse_kstat_lines(&text) {
            if module != "memory_cap" {
                continue;
            }
            let Ok(zone_id) = instance.parse::<u32>() else {
                continue;
            };
            let Ok(value) = raw.parse::<u64>() else {
                continue;
            };
            let e = acc.entry(zone_id).or_default();
            e.zone_id = zone_id;
            e.name = name.to_string();
            match stat {
                "rss" => e.rss = Some(value),
                "swap" => e.swap = Some(value),
                _ => {}
            }
        }
        let mut out: Vec<ZoneMem> = acc
            .into_values()
            .filter_map(|a| {
                Some(ZoneMem {
                    zone_id: a.zone_id,
                    zone_name: a.name,
                    rss_bytes: a.rss?,
                    swap_bytes: a.swap?,
                })
            })
            .collect();
        out.sort_by_key(|z| z.zone_id);
        Ok(out)
    }

    /// Read per-zone VFS bytes from `zone_vfs:::nread zone_vfs:::nwritten`.
    pub async fn disk_stats(&self) -> Result<Vec<ZoneDisk>, KstatError> {
        let text = self
            .run_kstat(&["zone_vfs:::nread", "zone_vfs:::nwritten"])
            .await?;
        #[derive(Default)]
        struct Acc {
            zone_id: u32,
            name: String,
            nread: Option<u64>,
            nwritten: Option<u64>,
        }
        let mut acc: std::collections::HashMap<u32, Acc> = std::collections::HashMap::new();
        for (module, instance, name, stat, raw) in parse_kstat_lines(&text) {
            if module != "zone_vfs" {
                continue;
            }
            let Ok(zone_id) = instance.parse::<u32>() else {
                continue;
            };
            let Ok(value) = raw.parse::<u64>() else {
                continue;
            };
            let e = acc.entry(zone_id).or_default();
            e.zone_id = zone_id;
            e.name = name.to_string();
            match stat {
                "nread" => e.nread = Some(value),
                "nwritten" => e.nwritten = Some(value),
                _ => {}
            }
        }
        let mut out: Vec<ZoneDisk> = acc
            .into_values()
            .filter_map(|a| {
                Some(ZoneDisk {
                    zone_id: a.zone_id,
                    zone_name: a.name,
                    read_bytes: a.nread?,
                    write_bytes: a.nwritten?,
                })
            })
            .collect();
        out.sort_by_key(|z| z.zone_id);
        Ok(out)
    }

    /// Read per-datalink RX/TX bytes from `link:::rbytes64 link:::obytes64`.
    pub async fn net_links(&self) -> Result<Vec<LinkStat>, KstatError> {
        let text = self
            .run_kstat(&["link:::rbytes64", "link:::obytes64"])
            .await?;
        #[derive(Default)]
        struct Acc {
            link: String,
            rx: Option<u64>,
            tx: Option<u64>,
        }
        let mut acc: std::collections::HashMap<String, Acc> = std::collections::HashMap::new();
        for (module, _instance, name, stat, raw) in parse_kstat_lines(&text) {
            if module != "link" {
                continue;
            }
            let Ok(value) = raw.parse::<u64>() else {
                continue;
            };
            let e = acc.entry(name.to_string()).or_default();
            e.link = name.to_string();
            match stat {
                "rbytes64" => e.rx = Some(value),
                "obytes64" => e.tx = Some(value),
                _ => {}
            }
        }
        let mut out: Vec<LinkStat> = acc
            .into_values()
            .filter_map(|a| {
                Some(LinkStat {
                    link: a.link,
                    rx_bytes: a.rx?,
                    tx_bytes: a.tx?,
                })
            })
            .collect();
        out.sort_by(|a, b| a.link.cmp(&b.link));
        Ok(out)
    }

    /// Read the system load average from `unix:0:system_misc:avenrun_*min`,
    /// divided by the kstat's 256x fixed-point scaling. Returns `None`
    /// if any of the three stats is missing.
    pub async fn load_avg(&self) -> Result<Option<LoadAvg>, KstatError> {
        let text = self
            .run_kstat(&[
                "unix:0:system_misc:avenrun_1min",
                "unix:0:system_misc:avenrun_5min",
                "unix:0:system_misc:avenrun_15min",
            ])
            .await?;
        let mut one = None;
        let mut five = None;
        let mut fifteen = None;
        for (module, _instance, name, stat, raw) in parse_kstat_lines(&text) {
            if module != "unix" || name != "system_misc" {
                continue;
            }
            let Ok(v) = raw.parse::<u64>() else { continue };
            let scaled = v as f64 / 256.0;
            match stat {
                "avenrun_1min" => one = Some(scaled),
                "avenrun_5min" => five = Some(scaled),
                "avenrun_15min" => fifteen = Some(scaled),
                _ => {}
            }
        }
        Ok(match (one, five, fifteen) {
            (Some(one), Some(five), Some(fifteen)) => Some(LoadAvg { one, five, fifteen }),
            _ => None,
        })
    }

    /// Read established-TCP counts per netstack from `tcp:::currEstab`.
    /// The kstat instance is the netstack id, which equals the zoneid
    /// for exclusive-IP zones (and 0 for the GZ).
    pub async fn tcp_estab(&self) -> Result<Vec<(u32, u64)>, KstatError> {
        let text = self.run_kstat(&["tcp:::currEstab"]).await?;
        let mut out = Vec::new();
        for (module, instance, name, stat, raw) in parse_kstat_lines(&text) {
            if module != "tcp" || name != "tcp" || stat != "currEstab" {
                continue;
            }
            let (Ok(zone_id), Ok(v)) = (instance.parse::<u32>(), raw.parse::<u64>()) else {
                continue;
            };
            out.push((zone_id, v));
        }
        out.sort_by_key(|(id, _)| *id);
        Ok(out)
    }

    /// Read ZFS ARC effectiveness + sizing from `zfs:0:arcstats`.
    /// A single `kstat -p zfs:0:arcstats` lists every stat for the
    /// kstat; we pick the fields the Cache·ARC view needs.
    pub async fn arcstats(&self) -> Result<ArcStats, KstatError> {
        let text = self.run_kstat(&["zfs:0:arcstats"]).await?;
        Ok(parse_arcstats(&text))
    }

    /// Run `kstat -p <selectors...>` and return stdout as text.
    /// Shared by the per-metric readers above.
    async fn run_kstat(&self, selectors: &[&str]) -> Result<String, KstatError> {
        let mut args: Vec<&str> = vec!["-p"];
        args.extend_from_slice(selectors);
        let output = tokio::process::Command::new(&self.bin)
            .args(&args)
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
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Iterate `kstat -p` output lines, yielding
/// `(module, instance, name, stat, value)` tuples. Skips malformed
/// lines. `name` may contain colons in theory (rare); we use
/// `splitn(4, ':')` so the stat is always the last component and the
/// name absorbs any extras.
fn parse_kstat_lines(text: &str) -> impl Iterator<Item = (&str, &str, &str, &str, &str)> {
    text.lines().filter_map(|line| {
        let (selector, raw) = line.split_once('\t')?;
        let raw = raw.trim();
        // module:instance:name...:stat -- split off module + instance
        // from the front and stat from the back so a name with a
        // stray colon doesn't shift the columns.
        let mut front = selector.splitn(3, ':');
        let module = front.next()?;
        let instance = front.next()?;
        let rest = front.next()?; // name...:stat
        let (name, stat) = rest.rsplit_once(':')?;
        Some((module, instance, name, stat, raw))
    })
}

/// Parse the tab-separated `kstat -p zones:::nsec_user zones:::nsec_sys
/// zones:::nsec_waitrq` output into one [`ZoneCpu`] per zone.
///
/// kstat prints lines of the form `module:instance:name:stat<TAB>value`.
/// We collect three stat values per zone keyed by `(instance, name)`;
/// zones missing any stat are dropped.
fn parse_zone_cpu(text: &str) -> Vec<ZoneCpu> {
    use std::collections::HashMap;

    #[derive(Default)]
    struct Acc {
        zone_id: u32,
        zone_name: String,
        user: Option<u64>,
        system: Option<u64>,
        iowait: Option<u64>,
    }

    // Key on the kstat instance (zoneid) -- it's stable per running
    // zone and, unlike the name field, isn't truncated.
    let mut acc: HashMap<u32, Acc> = HashMap::new();

    for line in text.lines() {
        let mut parts = line.splitn(2, '\t');
        let selector = parts.next().unwrap_or("");
        let raw = match parts.next() {
            Some(v) => v.trim(),
            None => continue,
        };
        let mut sel = selector.split(':');
        let module = sel.next().unwrap_or("");
        let instance = sel.next().unwrap_or("");
        let name = sel.next().unwrap_or("");
        let stat = sel.next().unwrap_or("");
        if module != "zones" {
            continue;
        }
        let zone_id: u32 = match instance.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let value: u64 = match raw.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let entry = acc.entry(zone_id).or_default();
        entry.zone_id = zone_id;
        entry.zone_name = name.to_string();
        match stat {
            "nsec_user" => entry.user = Some(value),
            "nsec_sys" => entry.system = Some(value),
            "nsec_waitrq" => entry.iowait = Some(value),
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(acc.len());
    for (_, a) in acc {
        if let (Some(u), Some(s), Some(w)) = (a.user, a.system, a.iowait) {
            out.push(ZoneCpu {
                zone_id: a.zone_id,
                zone_name: a.zone_name,
                user_ns: u,
                system_ns: s,
                iowait_ns: w,
            });
        }
    }
    out.sort_by_key(|z| z.zone_id);
    out
}

/// Parse `kstat -p zfs:0:arcstats` output into [`ArcStats`]. Unknown /
/// missing stats stay 0.
fn parse_arcstats(text: &str) -> ArcStats {
    let mut a = ArcStats::default();
    for (module, _instance, name, stat, raw) in parse_kstat_lines(text) {
        if module != "zfs" || name != "arcstats" {
            continue;
        }
        let Ok(v) = raw.parse::<u64>() else { continue };
        match stat {
            "hits" => a.hits = v,
            "misses" => a.misses = v,
            "l2_hits" => a.l2_hits = v,
            "l2_misses" => a.l2_misses = v,
            "size" => a.size = v,
            "c" => a.target = v,
            "c_max" => a.c_max = v,
            "mfu_size" => a.mfu_size = v,
            "mru_size" => a.mru_size = v,
            "metadata_size" => a.metadata_size = v,
            "l2_size" => a.l2_size = v,
            _ => {}
        }
    }
    a
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

    // Real kstat output: the name field is the zonename **truncated
    // to 30 bytes**, so a 36-char VM UUID comes back mangled (here
    // `a0f29ee3-0ec7-4e0c-9eca-f73323` -- 30 chars). The reliable
    // key is the instance number (zoneid).
    const ZONE_CPU_SAMPLE: &str = "\
zones:0:global:nsec_user\t1000
zones:0:global:nsec_sys\t200
zones:0:global:nsec_waitrq\t50
zones:1:a0f29ee3-0ec7-4e0c-9eca-f73323:nsec_user\t5000
zones:1:a0f29ee3-0ec7-4e0c-9eca-f73323:nsec_sys\t900
zones:1:a0f29ee3-0ec7-4e0c-9eca-f73323:nsec_waitrq\t100
zones:2:incomplete:nsec_user\t42";

    #[test]
    fn parses_zone_cpu_keyed_by_zone_id() {
        let zones = parse_zone_cpu(ZONE_CPU_SAMPLE);
        assert_eq!(zones.len(), 2, "incomplete zones should be dropped");
        // Sorted by zone_id.
        assert_eq!(zones[0].zone_id, 0);
        assert_eq!(zones[1].zone_id, 1);

        let global = &zones[0];
        assert_eq!(global.zone_name, "global");
        assert_eq!(global.user_ns, 1000);
        assert_eq!(global.system_ns, 200);
        assert_eq!(global.iowait_ns, 50);

        let vm = &zones[1];
        // The kstat name is the truncated prefix; callers resolve the
        // full UUID via zoneadm by zone_id.
        assert_eq!(vm.zone_name, "a0f29ee3-0ec7-4e0c-9eca-f73323");
        assert_eq!(vm.user_ns, 5000);
        assert_eq!(vm.system_ns, 900);
        assert_eq!(vm.iowait_ns, 100);
    }

    #[test]
    fn parses_arcstats() {
        let sample = "\
zfs:0:arcstats:hits\t9876543
zfs:0:arcstats:misses\t123456
zfs:0:arcstats:l2_hits\t4000
zfs:0:arcstats:l2_misses\t6000
zfs:0:arcstats:size\t103079215104
zfs:0:arcstats:c\t128849018880
zfs:0:arcstats:c_max\t137438953472
zfs:0:arcstats:mfu_size\t64424509440
zfs:0:arcstats:mru_size\t31138512896
zfs:0:arcstats:l2_size\t858993459200
zfs:0:arcstats:other_stat\t42";
        let a = parse_arcstats(sample);
        assert_eq!(a.hits, 9_876_543);
        assert_eq!(a.misses, 123_456);
        assert_eq!(a.l2_hits, 4_000);
        assert_eq!(a.size, 103_079_215_104);
        assert_eq!(a.target, 128_849_018_880);
        assert_eq!(a.c_max, 137_438_953_472);
        assert_eq!(a.mfu_size, 64_424_509_440);
        assert_eq!(a.l2_size, 858_993_459_200);
        // Absent stat (metadata_size) stays 0.
        assert_eq!(a.metadata_size, 0);
    }

    #[test]
    fn non_zone_modules_ignored() {
        let mixed = "\
unix:0:system_pages:availrmem\t100
zones:0:global:nsec_user\t1
zones:0:global:nsec_sys\t2
zones:0:global:nsec_waitrq\t3";
        let zones = parse_zone_cpu(mixed);
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].zone_id, 0);
        assert_eq!(zones[0].zone_name, "global");
    }
}
