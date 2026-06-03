// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Pure parsers for the `ipmitool` subcommands the collector runs, plus
//! the name→role / name→group mapping and the derived BMC security
//! posture. Everything here is text-in / struct-out and fixture-tested, so
//! correctness does not depend on live hardware.

use super::model::*;

// ── small line helpers ───────────────────────────────────────────────

/// Split a `Key : Value` line on its first colon.
fn kv(line: &str) -> Option<(&str, &str)> {
    line.split_once(':').map(|(k, v)| (k.trim(), v.trim()))
}

/// Split a `a | b | c` table row into trimmed columns.
fn cols(line: &str) -> Vec<&str> {
    line.split('|').map(str::trim).collect()
}

/// Parse a numeric token, returning `None` for `na` / blank / non-numeric.
fn num(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("na") {
        return None;
    }
    s.parse::<f64>().ok()
}

/// First whitespace-separated number in a string (e.g. "111 Watts" -> 111).
fn lead_num(s: &str) -> Option<f64> {
    s.trim()
        .split_whitespace()
        .next()
        .and_then(|t| t.parse().ok())
}

// ── mc info ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, PartialEq)]
pub struct McInfo {
    pub mfg: String,
    pub firmware: String,
    pub ipmi_version: String,
    pub product_id: String,
    pub vendor_is_dell: bool,
}

pub fn parse_mc_info(text: &str) -> McInfo {
    let mut mc = McInfo::default();
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        match k {
            "Manufacturer Name" => mc.mfg = v.to_string(),
            "Firmware Revision" => mc.firmware = v.to_string(),
            "IPMI Version" => mc.ipmi_version = v.to_string(),
            "Product ID" => {
                // "256 (0x0100)" -> "256 / 0x0100"
                mc.product_id = match v.split_once('(') {
                    Some((dec, hex)) => {
                        format!("{} / {}", dec.trim(), hex.trim_end_matches(')').trim())
                    }
                    None => v.trim().to_string(),
                };
            }
            _ => {}
        }
    }
    mc.vendor_is_dell = mc.mfg.to_ascii_uppercase().contains("DELL");
    mc
}

// ── sdr info (record count) ──────────────────────────────────────────

pub fn parse_sdr_record_count(text: &str) -> u32 {
    for line in text.lines() {
        if let Some((k, v)) = kv(line)
            && k == "Record Count"
        {
            return v.parse().unwrap_or(0);
        }
    }
    0
}

// ── sensor list (analog threshold sensors) ───────────────────────────

/// Parse `ipmitool sensor list`, keeping only the five analog kinds the
/// tab renders (temp / fan / voltage / current / power). Discrete rows
/// (`unit == "discrete"`) and utilization percentages are dropped — the
/// former are read from the compact SDR list, the latter aren't modeled.
pub fn parse_sensor_list(text: &str) -> Vec<ThresholdSensor> {
    let mut out = Vec::new();
    let mut cpu_temp_seen = 0u32;
    for line in text.lines() {
        let c = cols(line);
        if c.len() < 10 {
            continue;
        }
        let (name, unit) = (c[0], c[2]);
        let kind = match kind_for_unit(unit) {
            Some(k) => k,
            None => continue,
        };
        let Some(value) = num(c[1]) else { continue };

        // Bare "Temp" rows are the per-CPU dice; name + role them in order.
        let (display, role) = if name == "Temp" {
            cpu_temp_seen += 1;
            (
                format!("CPU{cpu_temp_seen} Temp"),
                format!("cpu{cpu_temp_seen}"),
            )
        } else {
            (name.to_string(), role_for_name(name))
        };

        out.push(ThresholdSensor {
            name: display,
            role,
            kind: kind.to_string(),
            entity: String::new(),
            value,
            unit: unit_symbol(unit).to_string(),
            status: norm_sensor_status(c[3]),
            th: Thresholds {
                lnr: num(c[4]),
                lcr: num(c[5]),
                lnc: num(c[6]),
                unc: num(c[7]),
                ucr: num(c[8]),
                unr: num(c[9]),
            },
            trend: vec![value],
        });
    }
    out
}

fn kind_for_unit(unit: &str) -> Option<&'static str> {
    match unit {
        "degrees C" => Some("temp"),
        "RPM" => Some("fan"),
        "Volts" => Some("voltage"),
        "Amps" => Some("current"),
        "Watts" => Some("power"),
        _ => None,
    }
}

fn unit_symbol(unit: &str) -> &str {
    match unit {
        "degrees C" => "°C",
        "RPM" => "RPM",
        "Volts" => "V",
        "Amps" => "A",
        "Watts" => "W",
        other => other,
    }
}

fn norm_sensor_status(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "ok" | "na" => "ok",
        "nc" => "nc",
        "cr" => "cr",
        "nr" => "nr",
        _ => "ok",
    }
    .to_string()
}

/// Map a well-known sensor name to its chassis-map role; fall back to a
/// slugified name so unknown sensors still get a stable key.
fn role_for_name(name: &str) -> String {
    match name {
        "Inlet Temp" => "inlet".into(),
        "Exhaust Temp" => "exhaust".into(),
        "CPU1 Temp" => "cpu1".into(),
        "CPU2 Temp" => "cpu2".into(),
        "Pwr Consumption" => "power".into(),
        "Voltage 1" => "psu1v".into(),
        "Voltage 2" => "psu2v".into(),
        "Current 1" => "psu1a".into(),
        "Current 2" => "psu2a".into(),
        n if n.starts_with("Fan") => n.to_ascii_lowercase().replace(' ', ""),
        n => n.to_ascii_lowercase().replace(' ', "-"),
    }
}

// ── compact SDR (discrete fault detectors) ───────────────────────────

/// Parse `ipmitool sdr elist compact`. Rows are
/// `Name | IDh | status | entity | state-string`. Only sensors that map to
/// one of the six UI groups are kept; the rest (bare Presence/Status rows,
/// SEL, watchdog) are dropped. Duplicate names are disambiguated by entity.
pub fn parse_sdr_compact(text: &str) -> Vec<DiscreteSensor> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    let mut seen: HashMap<String, u32> = HashMap::new();
    for line in text.lines() {
        let c = cols(line);
        if c.len() < 5 {
            continue;
        }
        let (name, status, entity, state) = (c[0], c[2], c[3], c[4]);
        let Some(group) = group_for_discrete(name) else {
            continue;
        };
        let (st, detail) = norm_discrete(status, state);

        // Disambiguate repeats (e.g. two "VCORE PG", entities 3.1/3.2).
        let count = seen.entry(name.to_string()).or_insert(0);
        *count += 1;
        let display = if *count > 1 {
            format!("{name} ({entity})")
        } else {
            name.to_string()
        };

        out.push(DiscreteSensor {
            group: group.to_string(),
            name: display,
            state: st.to_string(),
            detail,
        });
    }
    out
}

/// Classify a discrete sensor into one of the six UI groups, or `None` to
/// drop it (the long tail of generic Presence/Status rows).
fn group_for_discrete(name: &str) -> Option<&'static str> {
    let n = name;
    if n.ends_with("Redundancy") {
        Some("Redundancy")
    } else if n.starts_with("ECC")
        || n.starts_with("Mem")
        || n.starts_with("Memory")
        || n.contains("DIMM")
    {
        Some("Memory · RAS")
    } else if n.ends_with(" PG") || n.contains("PG Fail") {
        Some("Power rails")
    } else if n == "Intrusion"
        || n == "Dedicated NIC"
        // " Presence" (with a leading word) keeps "BP1 Presence" /
        // "Riser 2 Presence" but drops the bare per-CPU/DIMM "Presence" rows.
        || n.ends_with(" Presence")
        || n.ends_with("Cable Pres")
        || n.starts_with("PCIe Slot")
        || n.starts_with("Riser ")
        || n == "vFlash"
        || n.starts_with("Cable SAS")
    {
        Some("Presence")
    } else if n.starts_with("CPU")
        || n.starts_with("QPI")
        || n.starts_with("MRC")
        || n.starts_with("PCI")
        || n.starts_with("Link")
        || n.starts_with("Interconnect")
        || n.starts_with("Chipset")
    {
        Some("CPU · IO")
    } else if n.ends_with("Battery") {
        Some("Batteries")
    } else {
        None
    }
}

/// Normalize a compact-SDR (status, state-string) into (UI state, detail).
fn norm_discrete(status: &str, state: &str) -> (&'static str, String) {
    let st = match status.trim().to_ascii_lowercase().as_str() {
        "ok" => "ok",
        "nc" => "warn",
        "cr" | "nr" => "err",
        _ => "idle", // "ns" — no reading / disabled / absent
    };
    let detail = match state.trim() {
        "" => "ok".to_string(),
        "State Deasserted" => "power good".to_string(),
        "State Asserted" => "asserted".to_string(),
        "No Reading" | "Disabled" => "—".to_string(),
        other => other.to_string(),
    };
    (st, detail)
}

// ── FRU ──────────────────────────────────────────────────────────────

/// Parse `ipmitool fru print`. Each record opens with
/// `FRU Device Description : <desc> (ID n)`.
pub fn parse_fru(text: &str) -> Vec<FruDevice> {
    let mut out = Vec::new();
    let mut cur: Option<FruDevice> = None;

    let flush = |out: &mut Vec<FruDevice>, cur: Option<FruDevice>| {
        if let Some(f) = cur
            && !f.device.is_empty()
        {
            out.push(f);
        }
    };

    for line in text.lines() {
        // "Device not present (Timeout)" has no colon, so handle it before kv.
        if line.contains("not present") {
            if let Some(f) = cur.as_mut() {
                f.present = false;
            }
            continue;
        }
        let Some((k, v)) = kv(line) else { continue };
        match k {
            "FRU Device Description" => {
                flush(&mut out, cur.take());
                let (desc, id) = parse_fru_header(v);
                cur = Some(FruDevice {
                    id,
                    device: fru_label(&desc),
                    kind: fru_kind(&desc).to_string(),
                    present: true,
                    ..Default::default()
                });
            }
            _ => {
                let Some(f) = cur.as_mut() else { continue };
                match k {
                    "Board Product" | "Product Name" if f.model.is_empty() => {
                        f.model = v.to_string()
                    }
                    "Board Serial" if f.serial.is_empty() => f.serial = v.to_string(),
                    "Product Serial" if f.serial.is_empty() => f.serial = v.to_string(),
                    "Board Part Number" => f.part = v.to_string(),
                    "Board Mfg" | "Product Manufacturer" if f.mfg.is_empty() => {
                        f.mfg = v.to_string()
                    }
                    "Board Mfg Date" => f.date = fru_date(v),
                    _ => {}
                }
            }
        }
    }
    flush(&mut out, cur.take());
    out
}

/// "<desc> (ID n)" -> (desc, n).
fn parse_fru_header(v: &str) -> (String, u32) {
    if let Some((desc, rest)) = v.split_once("(ID") {
        let id = rest
            .trim()
            .trim_end_matches(')')
            .trim()
            .parse()
            .unwrap_or(0);
        (desc.trim().to_string(), id)
    } else {
        (v.trim().to_string(), 0)
    }
}

fn fru_label(desc: &str) -> String {
    // "Builtin FRU Device" reads better as "Mainboard"; keep PS1/PS2/etc.
    if desc.starts_with("Builtin") {
        "Mainboard".to_string()
    } else {
        desc.to_string()
    }
}

fn fru_kind(desc: &str) -> &'static str {
    let d = desc.to_ascii_uppercase();
    if desc.starts_with("Builtin") {
        "mainboard"
    } else if d.starts_with("PS") || d.contains("PWR") {
        "psu"
    } else if d.contains("NDC") || d.contains("NIC") {
        "nic"
    } else if d.contains("PERC") || d.contains("RAID") || d.contains("HBA") {
        "raid"
    } else if d.starts_with("BP") || d.contains("BACKPLANE") {
        "backplane"
    } else {
        "other"
    }
}

/// "Fri Jul 10 20:36:00 2015" -> "2015-07-10".
fn fru_date(v: &str) -> String {
    let p: Vec<&str> = v.split_whitespace().collect();
    if p.len() >= 5
        && let (Some(mon), Ok(day), Ok(year)) =
            (month_num(p[1]), p[2].parse::<u32>(), p[4].parse::<u32>())
    {
        return format!("{year:04}-{mon:02}-{day:02}");
    }
    v.to_string()
}

fn month_num(m: &str) -> Option<u32> {
    Some(match m {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

// ── SEL ──────────────────────────────────────────────────────────────

pub fn parse_sel_info(text: &str) -> (u32, u32, u32) {
    let (mut total, mut pct, mut cap) = (0, 0, 1024);
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        match k {
            "Entries" => total = v.parse().unwrap_or(0),
            "Percent Used" => pct = v.trim_end_matches('%').trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    // Free space + entries imply capacity; iDRAC caps at 1024 by default.
    if total > cap {
        cap = ((total / 1024) + 1) * 1024;
    }
    (total, pct, cap)
}

/// Parse `ipmitool sel elist`. Rows:
/// `id | MM/DD/YYYY | HH:MM:SS | source | desc | dir | [reading > threshold ...]`
pub fn parse_sel_elist(text: &str) -> Vec<SelEvent> {
    let mut out = Vec::new();
    for line in text.lines() {
        let c = cols(line);
        if c.len() < 6 {
            continue;
        }
        let Ok(id) = u32::from_str_radix(c[0].trim(), 16).or_else(|_| c[0].trim().parse()) else {
            continue;
        };
        let ts = sel_timestamp(c[1], c[2]);
        let source = c[3].to_string();
        let desc = c[4].to_string();
        let dir = if c[5].eq_ignore_ascii_case("asserted") {
            "asserted"
        } else {
            "deasserted"
        };
        let (reading, threshold) = c.get(6).map(|r| split_reading(r)).unwrap_or((None, None));
        let sev = sel_severity(dir, &desc);
        out.push(SelEvent {
            id,
            ts,
            source,
            desc,
            dir: dir.to_string(),
            sev: sev.to_string(),
            reading,
            threshold,
        });
    }
    out
}

/// "10/02/2025" + "22:07:09" -> RFC 3339 (UTC). Falls back to the raw
/// "date time" string if the shape is unexpected.
fn sel_timestamp(date: &str, time: &str) -> String {
    let d: Vec<&str> = date.split('/').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() == 3
        && t.len() == 3
        && let (Ok(mo), Ok(da), Ok(yr), Ok(h), Ok(mi), Ok(s)) = (
            d[0].parse::<u32>(),
            d[1].parse::<u32>(),
            d[2].parse::<i32>(),
            t[0].parse::<u32>(),
            t[1].parse::<u32>(),
            t[2].parse::<u32>(),
        )
        && let Some(dt) =
            chrono::NaiveDate::from_ymd_opt(yr, mo, da).and_then(|nd| nd.and_hms_opt(h, mi, s))
    {
        return chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc)
            .to_rfc3339();
    }
    format!("{} {}", date.trim(), time.trim())
}

/// "Reading 28 > Threshold 28 degrees C" -> (Some("28 °C"), Some("28 °C")).
fn split_reading(r: &str) -> (Option<String>, Option<String>) {
    if !r.contains("Threshold") {
        return (None, None);
    }
    let unit = if r.contains("degrees C") { " °C" } else { "" };
    let grab = |label: &str| -> Option<String> {
        let i = r.find(label)? + label.len();
        let rest = &r[i..];
        let n: String = rest
            .trim()
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
            .collect();
        (!n.is_empty()).then(|| format!("{n}{unit}"))
    };
    (grab("Reading"), grab("Threshold"))
}

fn sel_severity(dir: &str, desc: &str) -> &'static str {
    if dir == "deasserted" {
        return "ok";
    }
    let d = desc.to_ascii_lowercase();
    if d.contains("uncorrectable")
        || d.contains("critical")
        || d.contains("failure")
        || d.contains("fatal")
    {
        "err"
    } else {
        "warn"
    }
}

// ── chassis status ───────────────────────────────────────────────────

pub fn parse_chassis_status(text: &str) -> Chassis {
    let mut ch = Chassis {
        power: "on".into(),
        restore_policy: "previous".into(),
        restore_options: vec!["always-off".into(), "always-on".into(), "previous".into()],
        intrusion: "closed".into(),
        last_power_event: "AC power on".into(),
        boot_override: BootOverride {
            device: "none".into(),
            persistence: "next".into(),
            mode: "UEFI".into(),
        },
        boot_options: vec![
            "none".into(),
            "PXE".into(),
            "disk".into(),
            "BIOS setup".into(),
            "removable".into(),
        ],
        watchdog: Watchdog {
            present: true,
            running: false,
            action: "none".into(),
            action_options: vec![
                "none".into(),
                "reset".into(),
                "power-cycle".into(),
                "power-down".into(),
            ],
            countdown: None,
        },
        ..Default::default()
    };
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        let yes = v.eq_ignore_ascii_case("true");
        match k {
            "System Power" => ch.power = if v == "on" { "on".into() } else { "off".into() },
            "Power Restore Policy" => ch.restore_policy = v.to_string(),
            "Last Power Event" if !v.is_empty() => ch.last_power_event = v.to_string(),
            "Chassis Intrusion" => {
                ch.intrusion = if v.eq_ignore_ascii_case("active") {
                    "open".into()
                } else {
                    "closed".into()
                }
            }
            "Power Overload" => ch.faults.power_overload = yes,
            "Main Power Fault" => ch.faults.main_power_fault = yes,
            "Cooling/Fan Fault" => ch.faults.cooling_fault = yes,
            "Drive Fault" => ch.faults.drive_fault = yes,
            _ => {}
        }
    }
    ch
}

/// Fold `mc watchdog get` into a chassis watchdog.
pub fn parse_watchdog(text: &str, wd: &mut Watchdog) {
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        match k {
            "Watchdog Timer Is" => wd.running = v.eq_ignore_ascii_case("running"),
            "Watchdog Timer Actions" => {
                let a = v.to_ascii_lowercase();
                wd.action = if a.contains("no action") {
                    "none"
                } else if a.contains("reset") {
                    "reset"
                } else if a.contains("cycle") {
                    "power-cycle"
                } else if a.contains("down") {
                    "power-down"
                } else {
                    "none"
                }
                .into();
            }
            "Present Countdown" | "Initial Countdown" => {
                if wd.countdown.is_none() {
                    wd.countdown = lead_num(v).map(|n| n as u32);
                }
            }
            _ => {}
        }
    }
}

// ── LAN ──────────────────────────────────────────────────────────────

pub fn parse_lan_print(text: &str) -> BmcNet {
    let mut net = BmcNet::default();
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        match k {
            "IP Address Source" => {
                net.ip_source = if v.to_ascii_lowercase().contains("dhcp") {
                    "DHCP".into()
                } else {
                    "Static".into()
                }
            }
            "IP Address" => net.ip = v.to_string(),
            "Subnet Mask" => net.mask = v.to_string(),
            "MAC Address" if net.mac.is_empty() => net.mac = v.to_string(),
            "Default Gateway IP" => net.gateway = v.to_string(),
            "802.1q VLAN ID" => net.vlan = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    net
}

/// Pull the SNMP community, auth type, and cipher-suite list out of
/// `lan print` (kept separate from [`parse_lan_print`] so the net struct
/// stays small). Returns (snmp_community, auth_type, cipher_suites).
pub fn parse_lan_security(text: &str) -> (String, String, String) {
    let mut snmp = String::new();
    let mut auth = String::new();
    let mut cipher = String::new();
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        match k {
            "SNMP Community String" => snmp = v.to_string(),
            "Auth Type Enable" | "Auth Type Support" if auth.is_empty() => {
                auth = strongest_auth(v).to_string()
            }
            "RMCP+ Cipher Suites" => cipher = compact_cipher_list(v),
            _ => {}
        }
    }
    (snmp, auth, cipher)
}

fn strongest_auth(v: &str) -> &'static str {
    if v.contains("MD5") {
        "MD5"
    } else if v.contains("MD2") {
        "MD2"
    } else if v.to_ascii_lowercase().contains("password") {
        "password"
    } else {
        "none"
    }
}

/// "0,1,2,...,14" -> "0–14 advertised" when contiguous, else the raw list.
fn compact_cipher_list(v: &str) -> String {
    let nums: Vec<u32> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
    if nums.len() >= 2 {
        let (lo, hi) = (nums[0], *nums.last().unwrap());
        let contiguous = nums.iter().enumerate().all(|(i, n)| *n == lo + i as u32);
        if contiguous {
            return format!("{lo}–{hi} advertised");
        }
    }
    v.trim().to_string()
}

// ── users ────────────────────────────────────────────────────────────

/// Parse `user summary` (counts) + `user list` (rows). Only real accounts
/// (named, with access) are returned.
pub fn parse_users(summary: &str, list: &str) -> (Vec<BmcUser>, u32, u32) {
    let mut max = 0;
    let mut enabled = 0;
    for line in summary.lines() {
        if let Some((k, v)) = kv(line) {
            match k {
                "Maximum IDs" => max = v.parse().unwrap_or(0),
                "Enabled User Count" => enabled = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }

    let mut users = Vec::new();
    for line in list.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.is_empty() {
            continue;
        }
        let Ok(id) = toks[0].parse::<u32>() else {
            continue; // header / non-row
        };
        // Privilege is the trailing token(s): "NO ACCESS" is two words.
        let privilege = if line.to_ascii_uppercase().contains("NO ACCESS") {
            "NO ACCESS".to_string()
        } else {
            toks.last().unwrap().to_string()
        };
        // Name is token[1] unless that's a bool column (unused slot).
        let name = match toks.get(1) {
            Some(t) if *t != "true" && *t != "false" => t.to_string(),
            _ => String::new(),
        };
        if name.is_empty() || privilege == "NO ACCESS" {
            continue;
        }
        users.push(BmcUser {
            id,
            name,
            privilege,
            enabled: true,
        });
    }
    (users, max, enabled)
}

// ── SOL ──────────────────────────────────────────────────────────────

pub fn parse_sol_info(text: &str) -> Sol {
    let mut sol = Sol {
        bitrate: "115.2 kbps".into(),
        payload_port: 623,
        privilege: "ADMINISTRATOR".into(),
        ..Default::default()
    };
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        match k {
            "Enabled" => sol.enabled = v.eq_ignore_ascii_case("true"),
            "Force Encryption" => sol.encryption = v.eq_ignore_ascii_case("true"),
            "Privilege Level" => sol.privilege = v.to_string(),
            "Payload Port" => sol.payload_port = lead_num(v).map(|n| n as u32).unwrap_or(623),
            k if k.contains("Bit Rate") => {
                if let Some(n) = lead_num(v) {
                    sol.bitrate = format!("{n} kbps");
                }
            }
            _ => {}
        }
    }
    sol
}

pub fn parse_selftest(text: &str) -> String {
    for line in text.lines() {
        if let Some((k, v)) = kv(line)
            && k == "Selftest"
        {
            return v.to_string();
        }
    }
    "unknown".into()
}

// ── DCMI power + Dell delloem ────────────────────────────────────────

/// (instantaneous, min, max, avg) from `dcmi power reading`.
pub fn parse_dcmi_power(text: &str) -> (u32, u32, u32, u32) {
    let (mut inst, mut min, mut max, mut avg) = (0, 0, 0, 0);
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        let n = lead_num(v).map(|n| n as u32).unwrap_or(0);
        match k.trim() {
            "Instantaneous power reading" => inst = n,
            "Minimum during sampling period" => min = n,
            "Maximum during sampling period" => max = n,
            "Average power reading over sample period" => avg = n,
            _ => {}
        }
    }
    (inst, min, max, avg)
}

/// (cumulative_kwh, since, peak_w, peak_a) from `delloem powermonitor`.
pub fn parse_delloem_powermonitor(
    text: &str,
) -> (Option<f64>, Option<String>, Option<u32>, Option<f64>) {
    let (mut kwh, mut since, mut peak_w, mut peak_a) = (None, None, None, None);
    let mut section = "";
    for line in text.lines() {
        let Some((k, v)) = kv(line) else { continue };
        match k {
            "Statistic" => {
                section = if v.contains("Cumulative") {
                    "kwh"
                } else if v.contains("Peak Power") {
                    "pw"
                } else if v.contains("Peak Amperage") {
                    "pa"
                } else {
                    ""
                }
            }
            "Reading" if section == "kwh" => kwh = lead_num(v),
            "Start Time" if section == "kwh" && since.is_none() => since = Some(v.to_string()),
            "Peak Reading" if section == "pw" => peak_w = lead_num(v).map(|n| n as u32),
            "Peak Reading" if section == "pa" => peak_a = lead_num(v),
            _ => {}
        }
    }
    (kwh, since, peak_w, peak_a)
}

/// `delloem powermonitor powerconsumptionhistory` -> per-window avg/max/min.
/// Rows: `Average/Max/Min Power Consumption  <minute> <hour> <day> <week>`.
pub fn parse_delloem_history(text: &str) -> Option<PowerHistory> {
    let mut avg = [0u32; 4];
    let mut max = [0u32; 4];
    let mut min = [0u32; 4];
    let mut got = 0;
    for line in text.lines() {
        let want = if line.starts_with("Average Power") {
            Some(&mut avg)
        } else if line.starts_with("Max Power") {
            Some(&mut max)
        } else if line.starts_with("Min Power") {
            Some(&mut min)
        } else {
            None
        };
        let Some(slot) = want else { continue };
        // Pull the four "<n> W" numbers following the label.
        let nums: Vec<u32> = line
            .split_whitespace()
            .filter_map(|t| t.parse::<u32>().ok())
            .collect();
        if nums.len() >= 4 {
            slot[..].copy_from_slice(&nums[..4]);
            got += 1;
        }
    }
    if got < 3 {
        return None;
    }
    let win = |i: usize| PowerWindow {
        avg: avg[i],
        max: max[i],
        min: min[i],
    };
    Some(PowerHistory {
        minute: win(0),
        hour: win(1),
        day: win(2),
        week: win(3),
    })
}

/// `delloem mac list` -> per-LOM MAC map + the BMC's own MAC.
pub fn parse_delloem_mac(text: &str) -> Vec<NicMac> {
    let mut out = Vec::new();
    for line in text.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        // "iDRAC8 MAC Address 44:a8:..." — the BMC NIC.
        if line.contains("MAC Address")
            && line.contains(':')
            && let Some(mac) = toks.iter().find(|t| t.contains(':') && t.len() == 17)
        {
            out.push(NicMac {
                port: "iDRAC".into(),
                mac: (*mac).to_string(),
                enabled: true,
                bmc: true,
            });
            continue;
        }
        // "0   ec:f4:bb:cc:56:a4   Enabled"
        if toks.len() >= 2
            && let Ok(n) = toks[0].parse::<u32>()
            && toks[1].contains(':')
        {
            out.push(NicMac {
                port: format!("NIC {n}"),
                mac: toks[1].to_string(),
                enabled: toks
                    .get(2)
                    .map(|s| s.eq_ignore_ascii_case("enabled"))
                    .unwrap_or(true),
                bmc: false,
            });
        }
    }
    out
}

// ── derived BMC security posture ─────────────────────────────────────

/// Derive the first-class, actionable posture findings from the parsed BMC
/// security surface — the same checks the catalog's §10 flags.
pub fn derive_posture(bmc: &Bmc) -> Vec<PostureFinding> {
    let mut out = Vec::new();

    if bmc.cipher_suites.starts_with('0') {
        out.push(PostureFinding {
            severity: "err".into(),
            title: "Cipher suite 0 advertised (no auth, no encryption)".into(),
            detail: format!(
                "RMCP+ {} ; suite 0 lets a client authenticate with neither integrity nor confidentiality.",
                bmc.cipher_suites
            ),
            fix: Some("Restrict to suites 3 / 17 (HMAC-SHA + AES-128). lan set <ch> cipher_privs".into()),
        });
    }
    if bmc.snmp_community.eq_ignore_ascii_case("public") {
        out.push(PostureFinding {
            severity: "warn".into(),
            title: "SNMP community is ‘public’".into(),
            detail: "The default read community is set on the BMC LAN channel — anyone on the management VLAN can read sensors and inventory.".into(),
            fix: Some("Rotate to a non-default community or disable SNMP v1/v2c.".into()),
        });
    }
    let admins: Vec<&str> = bmc
        .users
        .iter()
        .filter(|u| u.privilege == "ADMINISTRATOR")
        .map(|u| u.name.as_str())
        .collect();
    if admins.len() > 1 {
        out.push(PostureFinding {
            severity: "warn".into(),
            title: format!("{} ADMINISTRATOR accounts ({})", admins.len(), admins.join(", ")),
            detail: "Multiple accounts hold full BMC admin out-of-band. A fleet should inventory and reconcile these against an owner.".into(),
            fix: Some("Confirm each admin is expected; otherwise disable or downgrade.".into()),
        });
    }
    if bmc.auth_type == "MD5" {
        out.push(PostureFinding {
            severity: "info".into(),
            title: "Auth type is MD5".into(),
            detail: "Acceptable but dated. Prefer the AES-backed cipher suites for new sessions."
                .into(),
            fix: None,
        });
    }
    if bmc.selftest.eq_ignore_ascii_case("passed") {
        out.push(PostureFinding {
            severity: "ok".into(),
            title: "BMC self-test passed".into(),
            detail: format!(
                "mc selftest reports no faults. SOL encryption is {}.",
                if bmc.sol.encryption {
                    "forced on"
                } else {
                    "off"
                }
            ),
            fix: None,
        });
    }
    out
}

// ── identity / bmc name helpers ──────────────────────────────────────

pub fn bmc_name_for(is_dell: bool, product_id: &str) -> String {
    if is_dell {
        if product_id.starts_with("256") {
            "iDRAC8".into()
        } else {
            "iDRAC9".into()
        }
    } else {
        "BMC".into()
    }
}

#[cfg(test)]
mod tests;
