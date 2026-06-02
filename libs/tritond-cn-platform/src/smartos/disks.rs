// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-disk inventory + SMART health + current iostat, assembled from
//! native illumos tools (no single source has it all):
//!
//!   * `diskinfo -Hp`  — device, vendor, product, size, ssd flag.
//!   * `iostat -En`    — serial, model, soft/hard/transport error counts,
//!                       and Predictive Failure Analysis (the native
//!                       SMART-trip signal, present even without smartctl).
//!   * `iostat -xn 1 2`— per-device busy% + service latency + IOPS for the
//!                       current 1s interval (the 2nd block).
//!   * `smartctl -d {sat|nvme} -j -a` — overall health, temperature,
//!                       wear%, reallocated sectors, power-on hours.
//!                       Optional: degrades to the native signals above
//!                       when smartmontools isn't installed.
//!
//! Merged by `cXtYdZ` device name. pool/vdev membership is layered on by
//! the caller from `zpool status`. SMART is slow-moving, so this runs on
//! the heartbeat tick, not the fast metrics tick.

use std::path::PathBuf;

/// Per-disk health snapshot. Every SMART-derived field is optional so a
/// node without smartmontools (or a disk that rejects the probe) still
/// reports inventory + native error counts.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct DiskHealth {
    pub device: String,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub size_bytes: Option<u64>,
    pub ssd: Option<bool>,
    /// `cXtYdZ` is direct-attach here; populated when a CN reports an
    /// enclosure slot (`diskinfo -P` LOCATION).
    pub slot: Option<String>,
    /// Pool + vdev this disk belongs to (filled by the caller from
    /// `zpool status`); `None` if not a pool member.
    pub pool: Option<String>,
    pub vdev: Option<String>,
    // ── SMART (smartctl) ─────────────────────────────────────────────
    /// `PASSED` / `FAILING` from `smart_status.passed`; `None` if SMART
    /// is unavailable.
    pub smart: Option<String>,
    pub temp_c: Option<u64>,
    /// Percent of rated write endurance consumed (0-100).
    pub wear_pct: Option<f64>,
    pub reallocated: Option<u64>,
    pub power_on_hours: Option<u64>,
    // ── native error counters (iostat -En) ───────────────────────────
    pub soft_errors: Option<u64>,
    pub hard_errors: Option<u64>,
    pub transport_errors: Option<u64>,
    /// Predictive Failure Analysis count -- nonzero is the firmware's
    /// own "this drive is dying" signal.
    pub pfa: Option<u64>,
    // ── current iostat (iostat -xn 1 2) ──────────────────────────────
    pub busy_pct: Option<f64>,
    pub svc_t_ms: Option<f64>,
    pub read_ops: Option<f64>,
    pub write_ops: Option<f64>,
    pub read_kbps: Option<f64>,
    pub write_kbps: Option<f64>,
}

/// Default tool paths. `smartctl` ships via pkgsrc (sbin), so it isn't on
/// the base PATH -- probe the usual prefixes.
const DEFAULT_DISKINFO: &str = "/usr/bin/diskinfo";
const DEFAULT_IOSTAT: &str = "/usr/bin/iostat";
const SMARTCTL_CANDIDATES: &[&str] = &[
    "/opt/tools/sbin/smartctl",
    "/opt/local/sbin/smartctl",
    "/usr/sbin/smartctl",
];

#[derive(Debug, Clone)]
pub struct DiskTool {
    pub diskinfo_bin: PathBuf,
    pub iostat_bin: PathBuf,
    /// `None` when smartmontools isn't installed -- SMART fields stay
    /// empty and the native signals carry the health story.
    pub smartctl_bin: Option<PathBuf>,
}

impl Default for DiskTool {
    fn default() -> Self {
        let smartctl_bin = SMARTCTL_CANDIDATES
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists());
        Self {
            diskinfo_bin: PathBuf::from(DEFAULT_DISKINFO),
            iostat_bin: PathBuf::from(DEFAULT_IOSTAT),
            smartctl_bin,
        }
    }
}

impl DiskTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn run(bin: &PathBuf, args: &[&str]) -> Option<String> {
        let out = tokio::process::Command::new(bin)
            .args(args)
            .output()
            .await
            .ok()?;
        // iostat/smartctl return nonzero for benign conditions (e.g. a
        // device with SMART warnings); take stdout regardless and let
        // the parser decide.
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Collect a per-disk snapshot. Best-effort: any sub-tool failing
    /// just leaves the corresponding fields empty.
    pub async fn collect(&self) -> Vec<DiskHealth> {
        let inv = Self::run(&self.diskinfo_bin, &["-Hp"])
            .await
            .map(|t| parse_diskinfo(&t))
            .unwrap_or_default();

        let en = Self::run(&self.iostat_bin, &["-En"])
            .await
            .map(|t| parse_iostat_en(&t))
            .unwrap_or_default();
        let xn = Self::run(&self.iostat_bin, &["-xn", "1", "2"])
            .await
            .map(|t| parse_iostat_xn_last(&t))
            .unwrap_or_default();

        let mut out = Vec::with_capacity(inv.len());
        for d in inv {
            let mut h = DiskHealth {
                device: d.device.clone(),
                vendor: d.vendor,
                product: d.product,
                size_bytes: d.size_bytes,
                ssd: d.ssd,
                ..Default::default()
            };
            if let Some(e) = en.get(&d.device) {
                h.serial = e.serial.clone();
                h.soft_errors = e.soft;
                h.hard_errors = e.hard;
                h.transport_errors = e.transport;
                h.pfa = e.pfa;
                if h.product.is_none() {
                    h.product = e.product.clone();
                }
            }
            if let Some(p) = xn.get(&d.device) {
                h.busy_pct = Some(p.busy);
                h.svc_t_ms = Some(p.asvc_t);
                h.read_ops = Some(p.reads);
                h.write_ops = Some(p.writes);
                h.read_kbps = Some(p.kr);
                h.write_kbps = Some(p.kw);
            }
            if let Some(sm) = &self.smartctl_bin {
                let dflag = if d.is_nvme { "nvme" } else { "sat" };
                let dev = format!("/dev/rdsk/{}", d.device);
                if let Some(json) = Self::run(sm, &["-d", dflag, "-j", "-a", &dev]).await
                    && let Some(s) = parse_smartctl_json(&json)
                {
                    h.smart = s.smart;
                    h.temp_c = s.temp_c;
                    h.wear_pct = s.wear_pct;
                    h.reallocated = s.reallocated;
                    h.power_on_hours = s.power_on_hours;
                }
            }
            out.push(h);
        }
        out.sort_by(|a, b| a.device.cmp(&b.device));
        out
    }
}

// ── diskinfo -Hp ─────────────────────────────────────────────────────
// TYPE \t DISK \t VID \t PID \t SIZE(bytes) \t RMV \t SSD

#[derive(Debug, Default, Clone)]
struct InvRow {
    device: String,
    vendor: Option<String>,
    product: Option<String>,
    size_bytes: Option<u64>,
    ssd: Option<bool>,
    is_nvme: bool,
}

fn parse_diskinfo(text: &str) -> Vec<InvRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() < 7 {
            continue;
        }
        let device = c[1].trim().to_string();
        if device.is_empty() {
            continue;
        }
        rows.push(InvRow {
            device,
            vendor: non_empty(c[2]),
            product: non_empty(c[3]),
            size_bytes: c[4].trim().parse::<u64>().ok(),
            ssd: match c[6].trim() {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            },
            is_nvme: c[0].trim().eq_ignore_ascii_case("NVME"),
        });
    }
    rows
}

// ── iostat -En ───────────────────────────────────────────────────────
// Per-disk text block:
//   c1t0d1  Soft Errors: 0 Hard Errors: 0 Transport Errors: 0
//   Vendor: ATA  Product: INTEL ... Revision: 0110 Serial No: PHYF...
//   Size: ...
//   Media Error: 0 Device Not Ready: 0 No Device: 0 Recoverable: 0
//   Illegal Request: 4 Predictive Failure Analysis: 0

#[derive(Debug, Default, Clone)]
struct EnRow {
    serial: Option<String>,
    product: Option<String>,
    soft: Option<u64>,
    hard: Option<u64>,
    transport: Option<u64>,
    pfa: Option<u64>,
}

fn parse_iostat_en(text: &str) -> std::collections::HashMap<String, EnRow> {
    let mut map = std::collections::HashMap::new();
    let mut cur: Option<(String, EnRow)> = None;
    for line in text.lines() {
        // A device block starts at a non-indented line whose first token
        // looks like a device name and which carries the error summary.
        let starts = !line.starts_with(char::is_whitespace) && line.contains("Soft Errors:");
        if starts {
            if let Some((dev, row)) = cur.take() {
                map.insert(dev, row);
            }
            let dev = line.split_whitespace().next().unwrap_or("").to_string();
            let mut row = EnRow::default();
            row.soft = field_after(line, "Soft Errors:");
            row.hard = field_after(line, "Hard Errors:");
            row.transport = field_after(line, "Transport Errors:");
            cur = Some((dev, row));
            continue;
        }
        if let Some((_, row)) = cur.as_mut() {
            if let Some(s) = str_field_after(line, "Serial No:") {
                row.serial = Some(s);
            }
            // Product can be multi-word ("INTEL SSDSC2KB01"); take everything
            // between `Product:` and the next `Revision:` label.
            if let Some(rest) = line.split("Product:").nth(1) {
                let p = rest.split("Revision:").next().unwrap_or(rest).trim();
                if !p.is_empty() {
                    row.product = Some(p.to_string());
                }
            }
            if let Some(n) = field_after(line, "Predictive Failure Analysis:") {
                row.pfa = Some(n);
            }
        }
    }
    if let Some((dev, row)) = cur.take() {
        map.insert(dev, row);
    }
    map
}

// ── iostat -xn 1 2 ───────────────────────────────────────────────────
// Two blocks separated by a header; the 2nd is the 1s interval (current).
//   r/s  w/s  kr/s  kw/s  wait  actv  wsvc_t  asvc_t  %w  %b  device

#[derive(Debug, Default, Clone)]
struct XnRow {
    reads: f64,
    writes: f64,
    kr: f64,
    kw: f64,
    asvc_t: f64,
    busy: f64,
}

fn parse_iostat_xn_last(text: &str) -> std::collections::HashMap<String, XnRow> {
    // Split into blocks at each "extended device statistics" header and
    // keep only the last block -- the current interval.
    let blocks: Vec<&str> = text.split("extended device statistics").collect();
    let last = blocks.last().copied().unwrap_or("");
    let mut map = std::collections::HashMap::new();
    for line in last.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        // 10 numeric columns + device name.
        if f.len() != 11 || f[0] == "r/s" {
            continue;
        }
        let num = |i: usize| f[i].parse::<f64>().unwrap_or(0.0);
        let device = f[10].to_string();
        map.insert(
            device,
            XnRow {
                reads: num(0),
                writes: num(1),
                kr: num(2),
                kw: num(3),
                asvc_t: num(7),
                busy: num(9),
            },
        );
    }
    map
}

// ── smartctl -j ──────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
struct SmartRow {
    smart: Option<String>,
    temp_c: Option<u64>,
    wear_pct: Option<f64>,
    reallocated: Option<u64>,
    power_on_hours: Option<u64>,
}

fn parse_smartctl_json(text: &str) -> Option<SmartRow> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut r = SmartRow::default();

    r.smart = v
        .pointer("/smart_status/passed")
        .and_then(|p| p.as_bool())
        .map(|p| if p { "PASSED" } else { "FAILING" }.to_string());
    r.temp_c = v.pointer("/temperature/current").and_then(|t| t.as_u64());
    r.power_on_hours = v.pointer("/power_on_time/hours").and_then(|h| h.as_u64());

    // NVMe carries wear + temp in a dedicated log.
    if let Some(used) = v
        .pointer("/nvme_smart_health_information_log/percentage_used")
        .and_then(|u| u.as_f64())
    {
        r.wear_pct = Some(used);
    }

    // SATA/ATA: read the attribute table by name.
    if let Some(table) = v
        .pointer("/ata_smart_attributes/table")
        .and_then(|t| t.as_array())
    {
        for a in table {
            let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
            match name {
                // Normalized value counts down from 100; % consumed = 100 - value.
                "Media_Wearout_Indicator" | "Wear_Leveling_Count" | "SSD_Life_Left" => {
                    if r.wear_pct.is_none()
                        && let Some(val) = a.get("value").and_then(|x| x.as_f64())
                    {
                        r.wear_pct = Some((100.0 - val).clamp(0.0, 100.0));
                    }
                }
                "Reallocated_Sector_Ct" => {
                    r.reallocated = a.pointer("/raw/value").and_then(|x| x.as_u64());
                }
                _ => {}
            }
        }
    }
    Some(r)
}

// ── small parse helpers ──────────────────────────────────────────────

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Numeric value following a `label:` on a line (whitespace-delimited).
fn field_after(line: &str, label: &str) -> Option<u64> {
    let after = line.split(label).nth(1)?;
    after.split_whitespace().next()?.parse::<u64>().ok()
}

/// String value following a `label:` -- the rest of the line up to the
/// next two-space gap (handles `Serial No: PHYF...` then trailing space).
fn str_field_after(line: &str, label: &str) -> Option<String> {
    let after = line.split(label).nth(1)?.trim();
    let v = after.split_whitespace().next()?.to_string();
    if v.is_empty() { None } else { Some(v) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_diskinfo_rows() {
        let t = "SCSI\tc1t0d1\tATA\tINTEL SSDSC2KB01\t1920383410176\tno\tyes\n\
                 NVME\tc3t0d0\tNVMe\tSamsung PM9A3\t3840755982336\tno\tyes\n";
        let rows = parse_diskinfo(t);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].device, "c1t0d1");
        assert_eq!(rows[0].product.as_deref(), Some("INTEL SSDSC2KB01"));
        assert_eq!(rows[0].size_bytes, Some(1_920_383_410_176));
        assert_eq!(rows[0].ssd, Some(true));
        assert!(!rows[0].is_nvme);
        assert!(rows[1].is_nvme);
    }

    #[test]
    fn parse_iostat_en_block() {
        let t = "c1t0d1           Soft Errors: 0 Hard Errors: 2 Transport Errors: 0 \n\
                 Vendor: ATA      Product: INTEL SSDSC2KB01 Revision: 0110 Serial No: PHYF9126021X1P9 \n\
                 Size: 1920.38GB <1920383410176 bytes>\n\
                 Media Error: 0 Device Not Ready: 0 No Device: 0 Recoverable: 0 \n\
                 Illegal Request: 4 Predictive Failure Analysis: 1 \n\
                 c1t1d1           Soft Errors: 0 Hard Errors: 0 Transport Errors: 0 \n\
                 Vendor: ATA      Product: INTEL SSDSC2KB01 Revision: 0110 Serial No: PHYF9126000H1P9 \n";
        let m = parse_iostat_en(t);
        let a = m.get("c1t0d1").expect("c1t0d1");
        assert_eq!(a.serial.as_deref(), Some("PHYF9126021X1P9"));
        assert_eq!(a.product.as_deref(), Some("INTEL SSDSC2KB01"));
        assert_eq!(a.hard, Some(2));
        assert_eq!(a.pfa, Some(1));
        assert!(m.contains_key("c1t1d1"));
    }

    #[test]
    fn parse_iostat_xn_takes_last_block() {
        let t = "                    extended device statistics              \n\
    r/s    w/s   kr/s   kw/s wait actv wsvc_t asvc_t  %w  %b device\n\
    0.6   42.8    9.8  858.8  0.0  0.0    0.0    9.9   0  88 c1t0d1\n\
                    extended device statistics              \n\
    r/s    w/s   kr/s   kw/s wait actv wsvc_t asvc_t  %w  %b device\n\
    0.0    4.0    0.0   20.0  0.0  0.0    0.0    0.2   0   3 c1t0d1\n";
        let m = parse_iostat_xn_last(t);
        let r = m.get("c1t0d1").expect("c1t0d1");
        // Must be the SECOND block's values, not the first.
        assert!((r.busy - 3.0).abs() < f64::EPSILON, "busy={}", r.busy);
        assert!((r.asvc_t - 0.2).abs() < 1e-9);
        assert!((r.writes - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_smartctl_sata() {
        let json = r#"{
          "smart_status": { "passed": true },
          "temperature": { "current": 17 },
          "power_on_time": { "hours": 55140 },
          "ata_smart_attributes": { "table": [
            { "id": 5, "name": "Reallocated_Sector_Ct", "value": 100, "raw": { "value": 0 } },
            { "id": 233, "name": "Media_Wearout_Indicator", "value": 95, "raw": { "value": 0 } }
          ] }
        }"#;
        let r = parse_smartctl_json(json).expect("parse");
        assert_eq!(r.smart.as_deref(), Some("PASSED"));
        assert_eq!(r.temp_c, Some(17));
        assert_eq!(r.power_on_hours, Some(55140));
        assert_eq!(r.reallocated, Some(0));
        assert_eq!(r.wear_pct, Some(5.0)); // 100 - 95
    }

    #[test]
    fn parse_smartctl_nvme() {
        let json = r#"{
          "smart_status": { "passed": false },
          "nvme_smart_health_information_log": { "percentage_used": 12, "temperature": 40 }
        }"#;
        let r = parse_smartctl_json(json).expect("parse");
        assert_eq!(r.smart.as_deref(), Some("FAILING"));
        assert_eq!(r.wear_pct, Some(12.0));
    }
}
