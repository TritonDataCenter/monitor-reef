// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-band IPMI / BMC / DCMI collector.
//!
//! Reads the baseboard management controller over the local KCS interface
//! (`ipmitool` with no host/credentials — `dcmi discover` reports "In-band
//! KCS channel available") and assembles the vendor-neutral
//! [`HardwareReport`] the admin Hardware tab renders. This is the
//! read/telemetry path; out-of-band control (power, SOL, identify) is a
//! separate credentialed tritond surface.
//!
//! Every subcommand is best-effort: a failed probe leaves its section at
//! the default rather than failing the whole report, and [`collect`]
//! returns `None` only when the first probe (`mc info`) fails — i.e. there
//! is no BMC or no `ipmitool` (a dev laptop), so the heartbeat simply omits
//! the `hardware` field.
//!
//! The whole report is cached for [`CACHE_TTL`]; hardware state moves
//! slowly and the heartbeat can fire every 500ms on zone churn, so we must
//! not spawn ~16 `ipmitool` processes against the BMC that often.

pub mod model;
mod parse;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

pub use model::HardwareReport;

/// Default in-band `ipmitool` path on a SmartOS CN.
pub const DEFAULT_IPMITOOL_BIN: &str = "/usr/sbin/ipmitool";

/// How long a collected report is reused before re-reading the BMC. Bounds
/// KCS load to ~once a minute regardless of how often the heartbeat fires.
const CACHE_TTL: Duration = Duration::from_secs(60);

const ACCESS: &str = "in-band KCS + LAN (RMCP+)";

struct CacheEntry {
    at: Instant,
    report: HardwareReport,
}

/// Wrapper around the in-band `ipmitool`. Share behind an `Arc`; the
/// internal cache is shared across heartbeat ticks.
pub struct IpmiTool {
    bin: PathBuf,
    cache: Mutex<Option<CacheEntry>>,
}

impl Default for IpmiTool {
    fn default() -> Self {
        Self {
            bin: PathBuf::from(DEFAULT_IPMITOOL_BIN),
            cache: Mutex::new(None),
        }
    }
}

impl IpmiTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            cache: Mutex::new(None),
        }
    }

    /// Run `ipmitool <args>` and return trimmed stdout, or `None` if the
    /// tool is missing or exits non-zero (logged at debug — a CN without a
    /// BMC, or a subcommand the BMC doesn't implement, is expected).
    async fn run(&self, args: &[&str]) -> Option<String> {
        let out = tokio::process::Command::new(&self.bin)
            .args(args)
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            tracing::debug!(args = ?args, status = ?out.status, "ipmitool subcommand failed");
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Collect one hardware report, honoring the [`CACHE_TTL`]. Returns
    /// `None` when there is no reachable BMC (the heartbeat omits the field).
    pub async fn collect(&self) -> Option<HardwareReport> {
        {
            let cache = self.cache.lock().await;
            if let Some(e) = cache.as_ref()
                && e.at.elapsed() < CACHE_TTL
            {
                return Some(e.report.clone());
            }
        }
        let report = self.collect_fresh().await?;
        let mut cache = self.cache.lock().await;
        *cache = Some(CacheEntry {
            at: Instant::now(),
            report: report.clone(),
        });
        Some(report)
    }

    async fn collect_fresh(&self) -> Option<HardwareReport> {
        // First probe gates the rest: no BMC -> no hardware section.
        let mc_text = self.run(&["mc", "info"]).await?;
        let mc = parse::parse_mc_info(&mc_text);
        let is_dell = mc.vendor_is_dell;

        let mut report = HardwareReport {
            vendor: if is_dell {
                "dell".into()
            } else {
                "generic".into()
            },
            ..Default::default()
        };

        // ── sensors / discrete ──────────────────────────────────────
        if let Some(t) = self.run(&["sensor", "list"]).await {
            report.sensors = parse::parse_sensor_list(&t);
        }
        if let Some(t) = self.run(&["sdr", "elist", "compact"]).await {
            report.discrete = parse::parse_sdr_compact(&t);
        }

        // ── SEL ─────────────────────────────────────────────────────
        if let Some(t) = self.run(&["sel", "info"]).await {
            let (total, pct, cap) = parse::parse_sel_info(&t);
            report.sel_total = total;
            report.sel_pct_used = pct;
            report.sel_cap = cap;
        }
        if let Some(t) = self.run(&["sel", "elist"]).await {
            // Most-recent first, bounded so a full SEL can't bloat last_status.
            let mut sel = parse::parse_sel_elist(&t);
            sel.reverse();
            sel.truncate(25);
            report.sel = sel;
        }

        // ── chassis + watchdog ──────────────────────────────────────
        report.chassis = self
            .run(&["chassis", "status"])
            .await
            .map(|t| parse::parse_chassis_status(&t))
            .unwrap_or_default();
        if let Some(t) = self.run(&["mc", "watchdog", "get"]).await {
            parse::parse_watchdog(&t, &mut report.chassis.watchdog);
        }

        // ── BMC: net + security + users + sol + selftest + posture ──
        let mut bmc = model::Bmc::default();
        if let Some(t) = self.run(&["lan", "print", "1"]).await {
            bmc.net = parse::parse_lan_print(&t);
            let (snmp, auth, cipher) = parse::parse_lan_security(&t);
            bmc.snmp_community = snmp;
            bmc.auth_type = auth;
            bmc.cipher_suites = cipher;
        }
        bmc.net.nic_mode = if is_dell {
            self.run(&["delloem", "lan", "get"])
                .await
                .map(|t| t.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "dedicated".into())
        } else {
            "shared (LOM)".into()
        };
        if let (Some(summary), Some(list)) = (
            self.run(&["user", "summary", "1"]).await,
            self.run(&["user", "list", "1"]).await,
        ) {
            let (users, max, enabled) = parse::parse_users(&summary, &list);
            bmc.users = users;
            bmc.max_users = max;
            bmc.enabled_users = enabled;
        }
        bmc.sol = self
            .run(&["sol", "info", "1"])
            .await
            .map(|t| parse::parse_sol_info(&t))
            .unwrap_or_default();
        bmc.selftest = self
            .run(&["mc", "selftest"])
            .await
            .map(|t| parse::parse_selftest(&t))
            .unwrap_or_else(|| "unknown".into());
        bmc.posture = parse::derive_posture(&bmc);
        report.bmc = bmc;

        // ── FRU + NICs ──────────────────────────────────────────────
        let fru_text = self.run(&["fru", "print"]).await.unwrap_or_default();
        report.fru = parse::parse_fru(&fru_text);
        if is_dell {
            if let Some(t) = self.run(&["delloem", "mac", "list"]).await {
                let nics = parse::parse_delloem_mac(&t);
                if !nics.is_empty() {
                    report.nics = Some(nics);
                }
            }
        }

        // ── power ───────────────────────────────────────────────────
        report.power = self
            .collect_power(is_dell, &report.sensors, &report.fru)
            .await;

        // ── identity ────────────────────────────────────────────────
        let bios = self
            .run(&["mc", "getsysinfo", "system_fw_version"])
            .await
            .map(|t| t.trim().to_string())
            .filter(|s| !s.is_empty() && !s.contains("Invalid"))
            .unwrap_or_default();
        let sdr_records = self
            .run(&["sdr", "info"])
            .await
            .map(|t| parse::parse_sdr_record_count(&t))
            .unwrap_or(0);
        report.identity = build_identity(
            &mc,
            &report.fru,
            &report.bmc.net.ip,
            bios,
            sdr_records,
            &fru_text,
        );

        Some(report)
    }

    /// DCMI instantaneous reading + per-PSU AC input (from voltage/current
    /// sensors and PSU FRU) + Dell cumulative/peak/history.
    async fn collect_power(
        &self,
        is_dell: bool,
        sensors: &[model::ThresholdSensor],
        fru: &[model::FruDevice],
    ) -> model::Power {
        let mut power = model::Power {
            period: "since reset".into(),
            psus: build_psus(sensors, fru),
            cap: model::PowerCap {
                supported: true,
                enabled: false,
                limit_w: None,
                note: if is_dell {
                    "DCMI cap not configured (iDRAC Enterprise license)".into()
                } else {
                    "DCMI power cap supported but not configured".into()
                },
            },
            ..Default::default()
        };

        if let Some(t) = self.run(&["dcmi", "power", "reading"]).await {
            let (inst, min, max, avg) = parse::parse_dcmi_power(&t);
            power.instantaneous = inst;
            power.min = min;
            power.max = max;
            power.avg = avg;
        }
        // Fall back to the Pwr Consumption sensor if DCMI is unavailable.
        if power.instantaneous == 0
            && let Some(s) = sensors.iter().find(|s| s.role == "power")
        {
            power.instantaneous = s.value as u32;
        }

        if is_dell {
            if let Some(t) = self.run(&["delloem", "powermonitor"]).await {
                let (kwh, since, pw, pa) = parse::parse_delloem_powermonitor(&t);
                power.cumulative_kwh = kwh;
                power.cumulative_since = since;
                power.peak_w = pw;
                power.peak_a = pa;
            }
            if let Some(t) = self
                .run(&["delloem", "powermonitor", "powerconsumptionhistory"])
                .await
            {
                power.history = parse::parse_delloem_history(&t);
            }
        }

        power
    }
}

/// Build the two PSU rows from the Voltage/Current sensor pairs and the PSU
/// FRU entries (model + rated wattage).
fn build_psus(sensors: &[model::ThresholdSensor], fru: &[model::FruDevice]) -> Vec<model::Psu> {
    let psu_fru: Vec<&model::FruDevice> = fru
        .iter()
        .filter(|f| f.kind == "psu" && f.present)
        .collect();
    let val = |role: &str| sensors.iter().find(|s| s.role == role).map(|s| s.value);

    let mut out = Vec::new();
    for id in 1..=2u32 {
        let volts = val(&format!("psu{id}v")).unwrap_or(0.0);
        let amps = val(&format!("psu{id}a")).unwrap_or(0.0);
        let fru = psu_fru.get((id - 1) as usize);
        let psu_model = fru.map(|f| f.model.clone()).unwrap_or_default();
        // No PSU FRU and no input reading -> this PSU slot isn't populated.
        if psu_model.is_empty() && volts == 0.0 {
            continue;
        }
        out.push(model::Psu {
            id,
            label: format!("PS{id}"),
            rated: rated_watts(&psu_model),
            volts,
            amps,
            watts: (volts * amps).round() as u32,
            status: if volts > 0.0 {
                "ok".into()
            } else {
                "err".into()
            },
            model: psu_model,
        });
    }
    out
}

/// Pull the rated wattage out of a PSU model string, e.g.
/// "PWR SPLY,750W,RDNT,DELTA" -> 750, "PWS-1K23A-1R 1200W" -> 1200.
fn rated_watts(model: &str) -> u32 {
    for tok in model.split(|c: char| !c.is_ascii_alphanumeric()) {
        if let Some(num) = tok.strip_suffix(|c: char| c == 'W' || c == 'w')
            && let Ok(n) = num.parse::<u32>()
        {
            return n;
        }
    }
    0
}

fn build_identity(
    mc: &parse::McInfo,
    fru: &[model::FruDevice],
    bmc_ip: &str,
    bios: String,
    sdr_records: u32,
    fru_text: &str,
) -> model::Identity {
    let mainboard = fru.iter().find(|f| f.kind == "mainboard");
    let product = mainboard.map(|f| f.model.clone()).unwrap_or_default();
    let model_name = if mc.vendor_is_dell && !product.starts_with("Dell") {
        format!("Dell {product}")
    } else if product.is_empty() {
        mc.mfg.clone()
    } else {
        product
    };
    // Service tag is the builtin FRU's Product Serial (Dell's asset id),
    // distinct from the Board Serial we keep on the mainboard FRU row.
    let service_tag = product_serial(fru_text);

    model::Identity {
        model: model_name,
        form_factor: "rack".into(),
        service_tag,
        board_serial: mainboard.map(|f| f.serial.clone()).unwrap_or_default(),
        board_part: mainboard.map(|f| f.part.clone()).unwrap_or_default(),
        mfg: mc.mfg.clone(),
        product_id: mc.product_id.clone(),
        bios,
        bmc_name: parse::bmc_name_for(mc.vendor_is_dell, &mc.product_id),
        bmc_firmware: mc.firmware.clone(),
        bmc_aux_fw: None,
        ipmi_version: mc.ipmi_version.clone(),
        sdr_records,
        idrac_url: (mc.vendor_is_dell && !bmc_ip.is_empty()).then(|| format!("https://{bmc_ip}")),
        access: ACCESS.into(),
    }
}

/// First `Product Serial : <tag>` in the FRU dump (the builtin device's
/// service tag).
fn product_serial(fru_text: &str) -> Option<String> {
    fru_text.lines().find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.trim() == "Product Serial")
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty() && v != "—")
    })
}
