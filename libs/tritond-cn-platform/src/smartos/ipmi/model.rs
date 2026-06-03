// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Vendor-neutral CN hardware model, serialized into the heartbeat
//! `last_status.hardware` field.
//!
//! The field names match the admin console's `CnHardware` TypeScript model
//! 1:1 (`#[serde(rename_all = "camelCase")]`), so the frontend consumes
//! `last_status.hardware` directly with no adapter. The shape mirrors the
//! catalog's canonical model: identity / sensors / discrete / power /
//! chassis / bmc / fru / nics / SEL. The demo-only `crisis` overlay the
//! fixture carries has no server-side analog and is simply absent here.

use serde::Serialize;

/// Top-level hardware report. Every section is best-effort: a section the
/// collector could not read is left at its `Default` (empty vec / `None` /
/// zeroes) rather than dropping the whole report.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HardwareReport {
    /// `"dell"` (delloem surfaces available) or `"generic"`.
    pub vendor: String,
    pub identity: Identity,
    pub sensors: Vec<ThresholdSensor>,
    pub discrete: Vec<DiscreteSensor>,
    pub power: Power,
    pub chassis: Chassis,
    pub bmc: Bmc,
    pub fru: Vec<FruDevice>,
    /// Per-LOM MAC map (Dell delloem); `None` on a generic BMC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nics: Option<Vec<NicMac>>,
    pub sel_total: u32,
    pub sel_pct_used: u32,
    pub sel_cap: u32,
    pub sel: Vec<SelEvent>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub model: String,
    pub form_factor: String,
    pub service_tag: Option<String>,
    pub board_serial: String,
    pub board_part: String,
    pub mfg: String,
    pub product_id: String,
    pub bios: String,
    pub bmc_name: String,
    pub bmc_firmware: String,
    pub bmc_aux_fw: Option<String>,
    pub ipmi_version: String,
    pub sdr_records: u32,
    pub idrac_url: Option<String>,
    pub access: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Thresholds {
    pub lnr: Option<f64>,
    pub lcr: Option<f64>,
    pub lnc: Option<f64>,
    pub unc: Option<f64>,
    pub ucr: Option<f64>,
    pub unr: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdSensor {
    pub name: String,
    pub role: String,
    /// `temp` | `fan` | `voltage` | `current` | `power`.
    pub kind: String,
    pub entity: String,
    pub value: f64,
    pub unit: String,
    /// IPMI status: `ok` | `nc` | `cr` | `nr`.
    pub status: String,
    pub th: Thresholds,
    /// Recent samples for the inline sparkline. A single heartbeat read
    /// yields one sample; a real series is layered on from ClickHouse
    /// later (the frontend tolerates a short array).
    pub trend: Vec<f64>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct DiscreteSensor {
    pub group: String,
    pub name: String,
    /// `ok` | `warn` | `err` | `idle`.
    pub state: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Psu {
    pub id: u32,
    pub label: String,
    pub model: String,
    pub rated: u32,
    pub volts: f64,
    pub amps: f64,
    pub watts: u32,
    pub status: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct PowerWindow {
    pub avg: u32,
    pub max: u32,
    pub min: u32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct PowerHistory {
    pub minute: PowerWindow,
    pub hour: PowerWindow,
    pub day: PowerWindow,
    pub week: PowerWindow,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PowerCap {
    pub supported: bool,
    pub enabled: bool,
    pub limit_w: Option<u32>,
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Power {
    pub instantaneous: u32,
    pub min: u32,
    pub max: u32,
    pub avg: u32,
    pub period: String,
    pub cumulative_kwh: Option<f64>,
    pub cumulative_since: Option<String>,
    pub peak_w: Option<u32>,
    pub peak_wat: Option<String>,
    pub peak_a: Option<f64>,
    pub peak_aat: Option<String>,
    pub psus: Vec<Psu>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<PowerHistory>,
    pub cap: PowerCap,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ChassisFaults {
    #[serde(rename = "powerOverload")]
    pub power_overload: bool,
    #[serde(rename = "mainPowerFault")]
    pub main_power_fault: bool,
    #[serde(rename = "coolingFault")]
    pub cooling_fault: bool,
    #[serde(rename = "driveFault")]
    pub drive_fault: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BootOverride {
    pub device: String,
    /// `next` | `persistent`.
    pub persistence: String,
    pub mode: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Watchdog {
    pub present: bool,
    pub running: bool,
    pub action: String,
    pub action_options: Vec<String>,
    pub countdown: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Chassis {
    /// `on` | `off`.
    pub power: String,
    pub restore_policy: String,
    pub restore_options: Vec<String>,
    pub intrusion: String,
    pub last_power_event: String,
    pub faults: ChassisFaults,
    pub boot_override: BootOverride,
    pub boot_options: Vec<String>,
    pub identify_led: bool,
    pub watchdog: Watchdog,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BmcNet {
    pub ip_source: String,
    pub ip: String,
    pub mask: String,
    pub mac: String,
    pub gateway: String,
    pub vlan: u32,
    pub nic_mode: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct BmcUser {
    pub id: u32,
    pub name: String,
    #[serde(rename = "priv")]
    pub privilege: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sol {
    pub enabled: bool,
    pub bitrate: String,
    pub payload_port: u32,
    #[serde(rename = "priv")]
    pub privilege: String,
    pub encryption: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PostureFinding {
    /// `ok` | `info` | `warn` | `err`.
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Bmc {
    pub net: BmcNet,
    pub selftest: String,
    pub users: Vec<BmcUser>,
    pub max_users: u32,
    pub enabled_users: u32,
    pub sol: Sol,
    pub snmp_community: String,
    pub auth_type: String,
    pub cipher_suites: String,
    pub posture: Vec<PostureFinding>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct FruDevice {
    pub id: u32,
    pub device: String,
    /// `mainboard` | `psu` | `nic` | `raid` | `backplane` | `other`.
    pub kind: String,
    pub model: String,
    pub serial: String,
    pub part: String,
    pub mfg: String,
    pub date: String,
    pub present: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct NicMac {
    pub port: String,
    pub mac: String,
    pub enabled: bool,
    /// `true` only for the BMC's own LAN NIC. Always emitted; the frontend
    /// treats `false`/absent identically (`n.bmc &&`).
    pub bmc: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SelEvent {
    pub id: u32,
    /// RFC 3339 timestamp.
    pub ts: String,
    pub source: String,
    pub desc: String,
    /// `asserted` | `deasserted`.
    pub dir: String,
    /// `ok` | `warn` | `err`.
    pub sev: String,
    pub reading: Option<String>,
    pub threshold: Option<String>,
}
