// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Fixtures captured verbatim from `ipmitool` on monroe (Dell PowerEdge
//! R730 / iDRAC8). Trimmed to representative rows so each parser is
//! validated against real output without live hardware.

use super::*;

#[test]
fn mc_info_identifies_dell() {
    let s = "Device ID                 : 32
Firmware Revision         : 2.61
IPMI Version              : 2.0
Manufacturer ID           : 674
Manufacturer Name         : DELL Inc
Product ID                : 256 (0x0100)
Product Name              : Unknown (0x100)";
    let mc = parse_mc_info(s);
    assert!(mc.vendor_is_dell);
    assert_eq!(mc.firmware, "2.61");
    assert_eq!(mc.ipmi_version, "2.0");
    assert_eq!(mc.product_id, "256 / 0x0100");
    assert_eq!(bmc_name_for(mc.vendor_is_dell, &mc.product_id), "iDRAC8");
}

#[test]
fn sensor_list_parses_analog_and_skips_discrete() {
    let s = "SEL              | na         | discrete   | na    | na        | na        | na        | na        | na        | na
Fan Redundancy   | 0x0        | discrete   | 0x0180| na        | na        | na        | na        | na        | na
Inlet Temp       | 18.000     | degrees C  | ok    | na        | -7.000    | 10.000    | 28.000    | 47.000    | na
Temp             | 30.000     | degrees C  | ok    | na        | 3.000     | 8.000     | 86.000    | 91.000    | na
Temp             | 27.000     | degrees C  | ok    | na        | 3.000     | 8.000     | 86.000    | 91.000    | na
Fan1             | 3720.000   | RPM        | ok    | na        | 360.000   | 600.000   | na        | na        | na
Voltage 1        | 206.000    | Volts      | ok    | na        | na        | na        | na        | na        | na
Current 1        | 0.200      | Amps       | ok    | na        | na        | na        | na        | na        | na
CPU Usage        | 0.000      | percent    | ok    | na        | na        | na        | 101.000   | na        | na
Pwr Consumption  | 98.000     | Watts      | ok    | na        | na        | na        | 1792.000  | 1974.000  | na        ";
    let v = parse_sensor_list(s);
    // discrete SEL/Fan Redundancy + percent CPU Usage are dropped.
    let names: Vec<&str> = v.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Inlet Temp",
            "CPU1 Temp",
            "CPU2 Temp",
            "Fan1",
            "Voltage 1",
            "Current 1",
            "Pwr Consumption"
        ]
    );
    let inlet = &v[0];
    assert_eq!(inlet.role, "inlet");
    assert_eq!(inlet.kind, "temp");
    assert_eq!(inlet.unit, "°C");
    assert_eq!(inlet.value, 18.0);
    assert_eq!(inlet.th.lcr, Some(-7.0));
    assert_eq!(inlet.th.unc, Some(28.0));
    assert_eq!(inlet.th.ucr, Some(47.0));
    assert_eq!(inlet.th.lnr, None);
    // Bare "Temp" rows become CPU1/CPU2 in order.
    assert_eq!(v[1].role, "cpu1");
    assert_eq!(v[2].role, "cpu2");
    // 3=Fan1, 4=Voltage 1, 5=Current 1, 6=Pwr Consumption.
    assert_eq!(v[4].role, "psu1v");
    assert_eq!(v[5].role, "psu1a");
    assert_eq!(v[6].role, "power");
    // The five kinds rendered by the tab: temp x3, fan, voltage, current, power.
    assert_eq!(v.iter().filter(|s| s.kind == "temp").count(), 3);
}

#[test]
fn sdr_compact_groups_discretes_and_dedupes() {
    let s = "SEL              | 72h | ns  |  0.1 | No Reading
Intrusion        | 73h | ok  |  7.1 |
VCORE PG         | 23h | ok  |  3.1 | State Deasserted
VCORE PG         | 24h | ok  |  3.2 | State Deasserted
Fan Redundancy   | 75h | ok  |  7.1 | Fully Redundant
PS1 PG Fail      | 2Dh | ok  |  7.1 | State Deasserted
CMOS Battery     | 65h | ok  |  7.1 |
Presence         | 40h | ok  |  3.1 | Present
PCIe Slot1       | 90h | ns  |  7.1 | Disabled
CPU Machine Chk  | 00h | ok  |  3.1 | ";
    let v = parse_sdr_compact(s);
    let by = |g: &str| v.iter().filter(|d| d.group == g).count();
    assert_eq!(by("Redundancy"), 1);
    assert_eq!(by("Power rails"), 3); // 2x VCORE PG + PS1 PG Fail
    assert_eq!(by("Batteries"), 1);
    assert_eq!(by("CPU · IO"), 1);
    // Bare "Presence" + "SEL" are dropped; "Intrusion"/"PCIe Slot1" kept.
    assert!(v.iter().all(|d| d.name != "Presence"));
    assert!(v.iter().any(|d| d.name == "Intrusion"));
    // Duplicate "VCORE PG" disambiguated by entity.
    let vcore: Vec<&str> = v
        .iter()
        .filter(|d| d.name.starts_with("VCORE"))
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(vcore, vec!["VCORE PG", "VCORE PG (3.2)"]);
    // Healthy states -> ok; "State Deasserted" -> "power good".
    let fanred = v.iter().find(|d| d.name == "Fan Redundancy").unwrap();
    assert_eq!(fanred.state, "ok");
    assert_eq!(fanred.detail, "Fully Redundant");
    let vcore0 = v.iter().find(|d| d.name == "VCORE PG").unwrap();
    assert_eq!(vcore0.detail, "power good");
    // "ns" (disabled slot) -> idle.
    let slot = v.iter().find(|d| d.name == "PCIe Slot1").unwrap();
    assert_eq!(slot.state, "idle");
}

#[test]
fn fru_parses_present_and_absent() {
    let s = "FRU Device Description : Builtin FRU Device (ID 0)
 Board Mfg Date        : Fri Jul 10 20:36:00 2015
 Board Mfg             : DELL
 Board Product         : PowerEdge R730
 Board Serial          : CN7792157700UA
 Board Part Number     : 0599V5A06
 Product Serial        : D8B9T52

FRU Device Description : PS1 (ID 1)
 Board Mfg             : DELL
 Board Product         : PWR SPLY,750W,RDNT,DELTA
 Board Serial          : CN179725630XU9
 Board Part Number     : 0V1YJ6A00

FRU Device Description : BP0 (ID 12)
 Device not present (Timeout)

FRU Device Description : PERC1 (ID 10)
 Board Product         : Dell Storage Cntlr. H730P-Mini
 Board Serial          : CN7792164P0378";
    let v = parse_fru(s);
    assert_eq!(v.len(), 4);
    assert_eq!(v[0].device, "Mainboard");
    assert_eq!(v[0].kind, "mainboard");
    assert_eq!(v[0].model, "PowerEdge R730");
    assert_eq!(v[0].serial, "CN7792157700UA"); // board serial wins
    assert_eq!(v[0].date, "2015-07-10");
    assert_eq!(v[1].device, "PS1");
    assert_eq!(v[1].kind, "psu");
    assert!(!v[2].present); // BP0 absent
    assert_eq!(v[3].kind, "raid");
    assert_eq!(v.iter().filter(|f| f.present).count(), 3);
}

#[test]
fn sel_info_and_elist() {
    let info = "SEL Information
Version          : 1.5 (v1.5, v2 compliant)
Entries          : 62
Free Space       : 15392 bytes
Percent Used     : 6%";
    let (total, pct, cap) = parse_sel_info(info);
    assert_eq!(total, 62);
    assert_eq!(pct, 6);
    assert_eq!(cap, 1024);

    // Record IDs are hex in `sel elist` (3e == 62).
    let elist = "  3e | 10/02/2025 | 22:07:09 | Temperature Inlet Temp | Upper Non-critical going high | Deasserted | Reading 28 > Threshold 28 degrees C
  3d | 10/02/2025 | 17:33:30 | Memory DIMM A3 | Uncorrectable ECC | Asserted
  32 | 01/27/2022 | 22:54:22 | Power Supply Status | Power Supply AC lost | Asserted ";
    let ev = parse_sel_elist(elist);
    assert_eq!(ev.len(), 3);
    assert_eq!(ev[0].id, 0x3e);
    assert!(ev[0].ts.starts_with("2025-10-02T22:07:09"));
    assert_eq!(ev[0].dir, "deasserted");
    assert_eq!(ev[0].sev, "ok"); // deasserted is always ok
    assert_eq!(ev[0].reading.as_deref(), Some("28 °C"));
    assert_eq!(ev[0].threshold.as_deref(), Some("28 °C"));
    assert_eq!(ev[1].sev, "err"); // "Uncorrectable" asserted
    assert_eq!(ev[2].sev, "warn"); // AC lost asserted
}

#[test]
fn chassis_status_and_watchdog() {
    let s = "System Power         : on
Power Overload       : false
Power Restore Policy : previous
Last Power Event     :
Chassis Intrusion    : inactive
Cooling/Fan Fault    : false
Drive Fault          : false";
    let mut ch = parse_chassis_status(s);
    assert_eq!(ch.power, "on");
    assert_eq!(ch.intrusion, "closed");
    assert!(!ch.faults.cooling_fault);
    assert_eq!(ch.last_power_event, "AC power on"); // empty -> default

    let wd = "Watchdog Timer Use:     Reserved (0x00)
Watchdog Timer Is:      Stopped
Watchdog Timer Actions: No action (0x00)
Initial Countdown:      15 sec
Present Countdown:      15 sec";
    parse_watchdog(wd, &mut ch.watchdog);
    assert!(!ch.watchdog.running);
    assert_eq!(ch.watchdog.action, "none");
    assert_eq!(ch.watchdog.countdown, Some(15));
}

#[test]
fn lan_print_and_security() {
    let s = "IP Address Source       : Static Address
IP Address              : 172.16.201.127
Subnet Mask             : 255.255.255.0
MAC Address             : 44:a8:42:41:95:74
SNMP Community String   : public
Default Gateway IP      : 172.16.201.1
802.1q VLAN ID          : 201
RMCP+ Cipher Suites     : 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14
Auth Type Enable        : Callback : MD5";
    let net = parse_lan_print(s);
    assert_eq!(net.ip_source, "Static");
    assert_eq!(net.ip, "172.16.201.127");
    assert_eq!(net.mac, "44:a8:42:41:95:74");
    assert_eq!(net.vlan, 201);
    let (snmp, auth, cipher) = parse_lan_security(s);
    assert_eq!(snmp, "public");
    assert_eq!(auth, "MD5");
    assert_eq!(cipher, "0–14 advertised");
}

#[test]
fn users_keeps_only_real_accounts() {
    let summary = "Maximum IDs	    : 16
Enabled User Count  : 2
Fixed Name Count    : 1";
    let list = "ID  Name	     Callin  Link Auth	IPMI Msg   Channel Priv Limit
1                    true    false      false      NO ACCESS
2   root             true    true       true       ADMINISTRATOR
3   ncalvas          true    true       true       ADMINISTRATOR
4                    true    false      false      NO ACCESS";
    let (users, max, enabled) = parse_users(summary, list);
    assert_eq!(max, 16);
    assert_eq!(enabled, 2);
    assert_eq!(users.len(), 2);
    assert_eq!(users[0].name, "root");
    assert_eq!(users[0].privilege, "ADMINISTRATOR");
    assert_eq!(users[1].name, "ncalvas");
}

#[test]
fn sol_info_parses() {
    let s = "Set in progress                 : set-complete
Enabled                         : true
Force Encryption                : true
Privilege Level                 : ADMINISTRATOR
Volatile Bit Rate (kbps)        : 115.2
Payload Port                    : 623";
    let sol = parse_sol_info(s);
    assert!(sol.enabled);
    assert!(sol.encryption);
    assert_eq!(sol.bitrate, "115.2 kbps");
    assert_eq!(sol.payload_port, 623);
}

#[test]
fn dcmi_power_and_delloem() {
    let dcmi = "    Instantaneous power reading:                   111 Watts
    Minimum during sampling period:                 13 Watts
    Maximum during sampling period:                371 Watts
    Average power reading over sample period:      110 Watts";
    let (inst, min, max, avg) = parse_dcmi_power(dcmi);
    assert_eq!((inst, min, max, avg), (111, 13, 371, 110));

    let pm = "Statistic      : Cumulative Energy Consumption
Start Time     : Tue Apr 30 15:31:35 2019
Reading        : 6817.1 kWh
Statistic      : System Peak Power
Peak Reading   : 314 W
Statistic      : System Peak Amperage
Peak Reading   : 2.3 A";
    let (kwh, since, pw, pa) = parse_delloem_powermonitor(pm);
    assert_eq!(kwh, Some(6817.1));
    assert!(since.unwrap().contains("2019"));
    assert_eq!(pw, Some(314));
    assert_eq!(pa, Some(2.3));

    let hist = "Power Consumption History

Statistic                   Last Minute     Last Hour     Last Day     Last Week
Average Power Consumption   110 W           110 W         110 W        111 W
Max Power Consumption       119 W           119 W         145 W        212 W
Min Power Consumption       109 W           109 W         109 W        106 W";
    let h = parse_delloem_history(hist).expect("history");
    assert_eq!(h.minute.avg, 110);
    assert_eq!(h.week.max, 212);
    assert_eq!(h.day.max, 145);
    assert_eq!(h.week.min, 106);
}

#[test]
fn delloem_mac_map() {
    let s = "System LOMs
NIC Number	MAC Address		Status

0		ec:f4:bb:cc:56:a4	Enabled
1		ec:f4:bb:cc:56:a5	Enabled

iDRAC8 MAC Address 44:a8:42:41:95:74";
    let v = parse_delloem_mac(s);
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].port, "NIC 0");
    assert_eq!(v[0].mac, "ec:f4:bb:cc:56:a4");
    assert!(v[0].enabled);
    assert!(!v[0].bmc);
    assert_eq!(v[2].port, "iDRAC");
    assert!(v[2].bmc);
}

#[test]
fn posture_flags_real_findings() {
    let bmc = Bmc {
        snmp_community: "public".into(),
        auth_type: "MD5".into(),
        cipher_suites: "0–14 advertised".into(),
        selftest: "passed".into(),
        sol: Sol {
            encryption: true,
            ..Default::default()
        },
        users: vec![
            BmcUser {
                id: 2,
                name: "root".into(),
                privilege: "ADMINISTRATOR".into(),
                enabled: true,
            },
            BmcUser {
                id: 3,
                name: "ncalvas".into(),
                privilege: "ADMINISTRATOR".into(),
                enabled: true,
            },
        ],
        ..Default::default()
    };
    let p = derive_posture(&bmc);
    let sev = |t: &str| {
        p.iter()
            .find(|f| f.title.contains(t))
            .map(|f| f.severity.as_str())
    };
    assert_eq!(sev("Cipher suite 0"), Some("err"));
    assert_eq!(sev("SNMP community"), Some("warn"));
    assert_eq!(sev("ADMINISTRATOR accounts"), Some("warn"));
    assert_eq!(sev("Auth type is MD5"), Some("info"));
    assert_eq!(sev("self-test passed"), Some("ok"));
    // Actionable (warn+err) count drives the BMC sub-tab badge -> 3.
    assert_eq!(
        p.iter()
            .filter(|f| f.severity == "warn" || f.severity == "err")
            .count(),
        3
    );
}
