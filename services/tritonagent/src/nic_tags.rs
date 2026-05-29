// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Enumerate the CN's local nic_tags so they can be published in the
//! registration request.
//!
//! tritond owns the fleet-wide nic_tag *registry* (name -> id); the
//! agent reports, by NAME, which tags this CN's hardware provides plus
//! the physical link / MTU each lands on. tritond resolves the names
//! against the registry and writes the CN's
//! [`tritond_store::CnNicTagInventory`] (single-writer). An unresolved
//! name is dropped server-side — the agent never asserts an id.
//!
//! Source of truth is `nictagadm list -p` (parseable, custom delimiter
//! because MAC addresses contain the default `:`). The MTU comes from
//! sysinfo's `Network Interfaces` map keyed by the physical link, since
//! `nictagadm list` does not emit it. VLAN is not carried by either
//! surface in v1 and defaults to `0` (untagged); the authoritative
//! per-network VLAN lives on the operator-defined external Subnet.

use tritond_cn_platform::smartos::Sysinfo;
use tritond_client::types::RegisterNicTagProvision;

/// Delimiter passed to `nictagadm list -d`. Must not be `:` (the
/// default) because the MAC-address column contains colons.
const NICTAGADM_DELIM: char = ',';

/// Default link MTU when sysinfo does not carry one for the link.
const DEFAULT_MTU: u32 = 1500;

/// Etherstub / local tag types we do NOT report as fleet nic_tags.
/// These are pseudo-links (including proteus port tags) with no
/// physical egress, so they can never carry external traffic.
const SKIP_TYPES: &[&str] = &["etherstub", "overlay_rule"];

/// Enumerate the CN's local nic_tags for the registration request.
///
/// Best-effort: a failure to run `nictagadm` (or a non-illumos build)
/// yields an empty list rather than failing registration. An empty
/// list is a no-op server-side — it does not clobber a previously
/// published inventory.
pub fn enumerate(sysinfo: &Sysinfo) -> Vec<RegisterNicTagProvision> {
    let raw = match run_nictagadm_list() {
        Some(out) => out,
        None => return Vec::new(),
    };
    parse_nictagadm_list(&raw, sysinfo)
}

/// Run `nictagadm list -p -L -d ,` and return its stdout, or `None` on
/// a non-illumos build or any spawn / non-zero-exit failure.
fn run_nictagadm_list() -> Option<String> {
    if cfg!(not(target_os = "illumos")) {
        return None;
    }
    use std::process::Command;
    // -p parseable, -L exclude etherstubs, -d custom delimiter.
    let output = Command::new("nictagadm")
        .args(["list", "-p", "-L", "-d", &NICTAGADM_DELIM.to_string()])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        Ok(out) => {
            tracing::warn!(
                status = ?out.status,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "nictagadm list failed; publishing no nic_tags this registration",
            );
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not run nictagadm list; publishing no nic_tags");
            None
        }
    }
}

/// Parse `nictagadm list -p -d ,` output (`name,mac,link,type` per
/// line) into the registration DTO, enriching MTU from sysinfo.
///
/// Pure (no I/O) so it is unit-testable without illumos. Lines that
/// are blank, mistyped, an etherstub/overlay type, or carry an empty
/// name/link are skipped.
fn parse_nictagadm_list(raw: &str, sysinfo: &Sysinfo) -> Vec<RegisterNicTagProvision> {
    raw.lines()
        .filter_map(|line| parse_line(line, sysinfo))
        .collect()
}

fn parse_line(line: &str, sysinfo: &Sysinfo) -> Option<RegisterNicTagProvision> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // name,mac,link,type — split into at most 4 so a stray delimiter in
    // a later column never shifts the type field.
    let mut fields = line.splitn(4, NICTAGADM_DELIM);
    let name = fields.next()?.trim();
    let _mac = fields.next()?;
    let link = fields.next()?.trim();
    let ntype = fields.next().unwrap_or("").trim();

    if name.is_empty() || link.is_empty() {
        return None;
    }
    if SKIP_TYPES.contains(&ntype) {
        return None;
    }

    Some(RegisterNicTagProvision {
        name: name.to_string(),
        physical_nic: link.to_string(),
        vlan_id: 0,
        mtu: sysinfo_link_mtu(sysinfo, link).unwrap_or(DEFAULT_MTU),
    })
}

/// Look up a link's MTU in sysinfo's `Network Interfaces` map (keyed by
/// link name). Real CNs carry an `MTU` field per interface; older /
/// trimmed sysinfo blobs may not, in which case the caller falls back
/// to [`DEFAULT_MTU`].
fn sysinfo_link_mtu(sysinfo: &Sysinfo, link: &str) -> Option<u32> {
    let mtu = sysinfo
        .raw
        .get("Network Interfaces")?
        .as_object()?
        .get(link)?
        .get("MTU")?;
    // Real sysinfo reports MTU as a JSON string ("1500"); tolerate a
    // number too.
    if let Some(n) = mtu.as_u64() {
        return u32::try_from(n).ok();
    }
    mtu.as_str().and_then(|s| s.trim().parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sysinfo_with(interfaces: serde_json::Value) -> Sysinfo {
        Sysinfo {
            raw: serde_json::json!({ "Network Interfaces": interfaces }),
        }
    }

    #[test]
    fn parses_canonical_two_tag_output() {
        // Format taken verbatim from smartos-ui's nictagadm test fixture.
        let raw = "admin,d0:50:99:d0:85:34,igb1,normal\nexternal,d0:50:99:d0:85:34,igb2,normal";
        let sysinfo = sysinfo_with(serde_json::json!({
            "igb1": { "MTU": "1500" },
            "igb2": { "MTU": "9000" },
        }));
        let got = parse_nictagadm_list(raw, &sysinfo);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "admin");
        assert_eq!(got[0].physical_nic, "igb1");
        assert_eq!(got[0].mtu, 1500);
        assert_eq!(got[0].vlan_id, 0);
        assert_eq!(got[1].name, "external");
        assert_eq!(got[1].physical_nic, "igb2");
        assert_eq!(got[1].mtu, 9000);
    }

    #[test]
    fn skips_etherstubs_and_overlay_rules() {
        let raw = "external,00:00:00:00:00:01,igb2,normal\n\
                   proteus5,00:00:00:00:00:02,proteus5,etherstub\n\
                   ov0,00:00:00:00:00:03,ov0,overlay_rule";
        let sysinfo = sysinfo_with(serde_json::json!({ "igb2": { "MTU": "1500" } }));
        let got = parse_nictagadm_list(raw, &sysinfo);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "external");
    }

    #[test]
    fn defaults_mtu_when_sysinfo_lacks_link() {
        let raw = "external,00:00:00:00:00:01,igb2,normal";
        let sysinfo = sysinfo_with(serde_json::json!({}));
        let got = parse_nictagadm_list(raw, &sysinfo);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].mtu, DEFAULT_MTU);
    }

    #[test]
    fn tolerates_numeric_mtu() {
        let raw = "external,00:00:00:00:00:01,igb2,normal";
        let sysinfo = sysinfo_with(serde_json::json!({ "igb2": { "MTU": 9000 } }));
        let got = parse_nictagadm_list(raw, &sysinfo);
        assert_eq!(got[0].mtu, 9000);
    }

    #[test]
    fn skips_blank_and_malformed_lines() {
        // Blank line, a line with no link column, and a valid line.
        let raw = "\n\
                   ,00:00:00:00:00:01,,normal\n\
                   external,00:00:00:00:00:02,igb2,normal\n\
                   notenoughcolumns";
        let sysinfo = sysinfo_with(serde_json::json!({ "igb2": { "MTU": "1500" } }));
        let got = parse_nictagadm_list(raw, &sysinfo);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "external");
    }

    #[test]
    fn empty_output_is_empty_vec() {
        let sysinfo = sysinfo_with(serde_json::json!({}));
        assert!(parse_nictagadm_list("", &sysinfo).is_empty());
        assert!(parse_nictagadm_list("\n\n", &sysinfo).is_empty());
    }
}
