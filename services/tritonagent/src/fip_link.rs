// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Realize the per-CN external datalink a CN-terminated floating IP
//! egresses on.
//!
//! This mirrors the legacy SDC model (see `smartos-live` `VM.js`
//! `global-nic` + `vlan-id`): the **nic_tag is the physical-link
//! identity** (resolved to a datalink via `nictagadm list`), and the
//! **VLAN lives on the network** (the external subnet), not on the tag.
//! So one nic_tag (e.g. `external` -> `aggr0`) backs many VLANs.
//!
//! The agent realizes a per-`(physical-link, vlan)` vnic named `fipN`
//! (lowest-free index) over the nic_tag's link and attaches proteus +
//! the `<fip>/32` ipadm alias to it. One `fipN` serves every floating
//! IP on that `(link, vlan)`; the name carries no policy (the VLAN is a
//! property of the link it sits on, exactly like `net0`/`net1`).
//!
//! The shell-outs follow the same illumos-gated pattern as [`fip_net`]
//! (`ipadm`) and [`imds_arp`] (`arp`): the argument vectors are pure
//! and unit-tested; the exec itself no-ops on non-illumos builds.

use anyhow::{Context, Result, bail};

/// Delimiter for `nictagadm list -d` (must not be `:` — the MAC column
/// contains colons), matching `nic_tags::NICTAGADM_DELIM`.
const NICTAGADM_DELIM: &str = ",";

/// Prefix for the agent-managed external FIP vnics. Sequential
/// (`fip0`, `fip1`, ...); the index is allocated lowest-free per CN.
const FIP_LINK_PREFIX: &str = "fip";

/// Resolve a nic_tag NAME to its physical datalink via `nictagadm list`
/// (the legacy `name,mac,link,type` surface). For an aggr/normal tag
/// the `link` column is the datalink (e.g. `aggr0`); MAC-only tags
/// (link `-`) are unsupported in v1.
#[cfg(target_os = "illumos")]
fn resolve_nic_tag_link(nic_tag: &str) -> Result<String> {
    use std::process::Command;
    // `-L` excludes etherstubs (matching `nic_tags::enumerate`) so a
    // pseudo-link tag can never shadow the external nic_tag and get a
    // vnic created over a link with no physical egress.
    let out = Command::new("/usr/bin/nictagadm")
        .args(["list", "-p", "-L", "-d", NICTAGADM_DELIM])
        .output()
        .with_context(|| "run nictagadm list to resolve external nic_tag link")?;
    if !out.status.success() {
        bail!(
            "nictagadm list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_nic_tag_link(&raw, nic_tag).ok_or_else(|| {
        anyhow::anyhow!("nic_tag {nic_tag:?} resolves to no physical link in nictagadm list")
    })
}

/// Pure parse: find the physical datalink for `nic_tag` in
/// `name,mac,link,type` output. Returns `None` if the tag is absent or
/// its link column is empty / `-` (a MAC-only tag, unsupported in v1).
fn parse_nic_tag_link(raw: &str, nic_tag: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        let mut fields = line.trim().splitn(4, NICTAGADM_DELIM);
        let name = fields.next()?.trim();
        let _mac = fields.next()?;
        let link = fields.next()?.trim();
        if name == nic_tag && !link.is_empty() && link != "-" {
            Some(link.to_string())
        } else {
            None
        }
    })
}

/// Is `link` one of our managed `fipN` vnics (prefix + all-digit tail)?
fn is_fip_link(link: &str) -> bool {
    link.strip_prefix(FIP_LINK_PREFIX)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Lowest-free `fipN` index not already taken by an existing datalink
/// name in `links` (regardless of which link/vlan it sits on, so two
/// external VLANs never collide on a name).
fn next_fip_index(links: &[String]) -> u32 {
    let used: std::collections::BTreeSet<u32> = links
        .iter()
        .filter_map(|l| l.strip_prefix(FIP_LINK_PREFIX)?.parse().ok())
        .collect();
    (0u32..).find(|n| !used.contains(n)).unwrap_or(u32::MAX)
}

/// `dladm create-vnic -l <phys> [-v <vlan>] fip<N>` argument vector.
/// `vlan == 0` means untagged (no `-v`), matching legacy `VM.js` which
/// only sets `vlan-id` when non-zero.
fn create_vnic_args(phys: &str, vlan: u16, name: &str) -> Vec<String> {
    let mut args = vec![
        "create-vnic".to_string(),
        "-l".to_string(),
        phys.to_string(),
    ];
    if vlan != 0 {
        args.push("-v".to_string());
        args.push(vlan.to_string());
    }
    args.push(name.to_string());
    args
}

/// One `(link, over, vid)` row from `dladm show-vnic -p -o link,over,vid`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VnicRow {
    link: String,
    over: String,
    vid: u16,
}

/// Pure parse of `dladm show-vnic -p -o link,over,vid`. The `-p` form
/// is `:`-separated; none of these three columns contains a `:`.
fn parse_show_vnic(raw: &str) -> Vec<VnicRow> {
    raw.lines()
        .filter_map(|line| {
            let mut f = line.trim().split(':');
            let link = f.next()?.to_string();
            let over = f.next()?.to_string();
            let vid = f.next()?.parse().ok()?;
            Some(VnicRow { link, over, vid })
        })
        .collect()
}

/// Find an existing managed `fipN` vnic sitting over `phys` carrying
/// `vlan` (0 = untagged) among the parsed `rows`.
fn find_in_rows(rows: &[VnicRow], phys: &str, vlan: u16) -> Option<String> {
    rows.iter()
        .find(|r| is_fip_link(&r.link) && r.over == phys && r.vid == vlan)
        .map(|r| r.link.clone())
}

#[cfg(target_os = "illumos")]
fn show_vnic_rows() -> Result<Vec<VnicRow>> {
    use std::process::Command;
    let out = Command::new("/usr/sbin/dladm")
        .args(["show-vnic", "-p", "-o", "link,over,vid"])
        .output()
        .with_context(|| "run dladm show-vnic")?;
    if !out.status.success() {
        bail!(
            "dladm show-vnic failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_show_vnic(&String::from_utf8_lossy(&out.stdout)))
}

/// Resolve `nic_tag` + `vlan_id` to the external datalink, creating the
/// `fipN` vnic over the nic_tag's physical link if one for this
/// `(link, vlan)` does not already exist. Idempotent: a second claim on
/// the same `(link, vlan)` reuses the existing vnic.
#[cfg(target_os = "illumos")]
pub fn realize(nic_tag: &str, vlan_id: Option<u16>) -> Result<String> {
    use std::process::Command;
    let phys = resolve_nic_tag_link(nic_tag)?;
    let vlan = vlan_id.unwrap_or(0);

    let rows = show_vnic_rows()?;
    if let Some(existing) = find_in_rows(&rows, &phys, vlan) {
        return Ok(existing);
    }

    let existing_links: Vec<String> = rows.into_iter().map(|r| r.link).collect();
    let name = format!("{FIP_LINK_PREFIX}{}", next_fip_index(&existing_links));
    let args = create_vnic_args(&phys, vlan, &name);
    let out = Command::new("/usr/sbin/dladm")
        .args(&args)
        .output()
        .with_context(|| format!("run dladm create-vnic for external FIP link {name}"))?;
    if out.status.success() {
        tracing::info!(nic_tag, phys, vlan, link = %name, "fip-link: created external vnic");
        return Ok(name);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // A concurrent claim may have created it; re-find by (link, vlan).
    if stderr.contains("already exists") || stderr.contains("object already exists") {
        if let Some(existing) = find_in_rows(&show_vnic_rows()?, &phys, vlan) {
            return Ok(existing);
        }
    }
    bail!(
        "dladm create-vnic {name} -l {phys} vlan {vlan} failed: {}",
        stderr.trim()
    );
}

/// Find the existing external datalink for `nic_tag` + `vlan_id`
/// WITHOUT creating one (used on release to locate the alias's link).
/// `Ok(None)` = the nic_tag resolved but no `fipN` vnic matches (the
/// link is genuinely gone — release is idempotent). `Err` = the
/// `nictagadm`/`dladm` query itself failed, which the caller must treat
/// as fail-stop rather than "no link", so a transient hiccup does not
/// strand the `<fip>/32` alias as a stale ARP responder.
#[cfg(target_os = "illumos")]
pub fn find(nic_tag: &str, vlan_id: Option<u16>) -> Result<Option<String>> {
    let phys = resolve_nic_tag_link(nic_tag)?;
    let rows = show_vnic_rows()?;
    Ok(find_in_rows(&rows, &phys, vlan_id.unwrap_or(0)))
}

// Non-illumos stubs so the crate builds (and unit tests run) on the
// dev host. The pure parsers above are exercised by the tests below.
#[cfg(not(target_os = "illumos"))]
pub fn realize(_nic_tag: &str, _vlan_id: Option<u16>) -> Result<String> {
    bail!("external FIP link realization is only available on illumos")
}

#[cfg(not(target_os = "illumos"))]
pub fn find(_nic_tag: &str, _vlan_id: Option<u16>) -> Result<Option<String>> {
    Ok(None)
}

/// Resolve the MAC of `link` (the external FIP vnic) via
/// `dladm show-vnic`, for seeding the kmod's outbound `ExternalTx`
/// source MAC. illumos prints MAC bytes without leading zeros
/// (`2:8:20:98:b1:de`), so [`parse_mac`] tolerates 1- or 2-char bytes.
#[cfg(target_os = "illumos")]
pub fn link_mac(link: &str) -> Result<[u8; 6]> {
    use std::process::Command;
    let out = Command::new("/usr/sbin/dladm")
        .args(["show-vnic", link, "-o", "macaddress", "-p"])
        .output()
        .with_context(|| format!("dladm show-vnic {link} -o macaddress"))?;
    if !out.status.success() {
        bail!(
            "dladm show-vnic {link} macaddress failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse_mac(String::from_utf8_lossy(&out.stdout).trim())
        .ok_or_else(|| anyhow::anyhow!("could not parse MAC for external link {link}"))
}

#[cfg(not(target_os = "illumos"))]
pub fn link_mac(_link: &str) -> Result<[u8; 6]> {
    bail!("link MAC resolution is only available on illumos")
}

/// Resolve the 802.1Q VLAN id of `link` (the external FIP vnic) via
/// `dladm show-vnic`, for seeding the kmod's outbound `ExternalTx` tag.
/// The fastpath tx does not auto-insert the tag, so the kmod stamps it
/// from this value. Returns `0` (untagged) when dladm reports no VID.
#[cfg(target_os = "illumos")]
pub fn link_vlan(link: &str) -> Result<u16> {
    use std::process::Command;
    let out = Command::new("/usr/sbin/dladm")
        .args(["show-vnic", link, "-o", "vid", "-p"])
        .output()
        .with_context(|| format!("dladm show-vnic {link} -o vid"))?;
    if !out.status.success() {
        bail!(
            "dladm show-vnic {link} vid failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    s.parse::<u16>()
        .with_context(|| format!("could not parse VID '{s}' for external link {link}"))
}

#[cfg(not(target_os = "illumos"))]
pub fn link_vlan(_link: &str) -> Result<u16> {
    bail!("link VLAN resolution is only available on illumos")
}

/// Upstream FlatL2 gateway MAC for outbound FIP frames, from the
/// `TRITONAGENT_EXTERNAL_GATEWAY_MAC` deployment-config env. This is a
/// per-CN stopgap; the production source is a `gateway_mac` on the
/// external subnet, threaded through the FipClaim (followup). `None`
/// when unset / unparseable -> the kmod ExternalTx fails closed.
pub fn env_gateway_mac() -> Option<[u8; 6]> {
    parse_mac(
        std::env::var("TRITONAGENT_EXTERNAL_GATEWAY_MAC")
            .ok()?
            .trim(),
    )
}

/// Parse a colon-separated MAC (`aa:bb:..` or illumos `a:b:..`) into 6
/// bytes. Returns `None` on any malformed input.
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 2 {
            return None;
        }
        out[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nic_tag_link_from_aggr_tag() {
        // monroe's real shape: name,mac,link,type
        let raw = "external,-,aggr0,aggr\ninternal,-,aggr0,aggr\nadmin,-,aggr0,aggr\n";
        assert_eq!(
            parse_nic_tag_link(raw, "external").as_deref(),
            Some("aggr0")
        );
        assert_eq!(
            parse_nic_tag_link(raw, "internal").as_deref(),
            Some("aggr0")
        );
        assert_eq!(parse_nic_tag_link(raw, "nope"), None);
    }

    #[test]
    fn skips_mac_only_tag_with_empty_link() {
        // A MAC-based tag has link `-`; unsupported in v1 -> None.
        let raw = "external,00:50:56:3d:a7:95,-,normal\n";
        assert_eq!(parse_nic_tag_link(raw, "external"), None);
    }

    #[test]
    fn identifies_fip_links() {
        assert!(is_fip_link("fip0"));
        assert!(is_fip_link("fip17"));
        assert!(!is_fip_link("fip"));
        assert!(!is_fip_link("fipa"));
        assert!(!is_fip_link("external0"));
        assert!(!is_fip_link("net0"));
    }

    #[test]
    fn allocates_lowest_free_index() {
        assert_eq!(next_fip_index(&[]), 0);
        assert_eq!(
            next_fip_index(&["fip0".into(), "fip1".into(), "net0".into()]),
            2
        );
        // Holes are filled lowest-first.
        assert_eq!(next_fip_index(&["fip0".into(), "fip2".into()]), 1);
    }

    #[test]
    fn create_vnic_args_tagged_and_untagged() {
        assert_eq!(
            create_vnic_args("aggr0", 2003, "fip0"),
            vec!["create-vnic", "-l", "aggr0", "-v", "2003", "fip0"]
        );
        // vlan 0 = untagged: no -v.
        assert_eq!(
            create_vnic_args("aggr0", 0, "fip0"),
            vec!["create-vnic", "-l", "aggr0", "fip0"]
        );
    }

    #[test]
    fn finds_matching_vnic_by_over_and_vid() {
        let raw = "fip0:aggr0:2003\nfip1:aggr0:109\nnet0:aggr0:0\n";
        let rows = parse_show_vnic(raw);
        assert_eq!(find_in_rows(&rows, "aggr0", 2003).as_deref(), Some("fip0"));
        assert_eq!(find_in_rows(&rows, "aggr0", 109).as_deref(), Some("fip1"));
        // No fipN over aggr0 with vid 999.
        assert_eq!(find_in_rows(&rows, "aggr0", 999), None);
        // net0 is not a managed fip link even though it matches over/vid.
        assert_eq!(find_in_rows(&rows, "aggr0", 0), None);
    }

    #[test]
    fn parses_macs_illumos_and_standard_forms() {
        // illumos `dladm` omits leading zeros per byte.
        assert_eq!(
            parse_mac("2:8:20:98:b1:de"),
            Some([0x02, 0x08, 0x20, 0x98, 0xb1, 0xde])
        );
        // standard zero-padded (e.g. a VRRP VIP gateway MAC).
        assert_eq!(
            parse_mac("00:00:5e:00:01:c9"),
            Some([0x00, 0x00, 0x5e, 0x00, 0x01, 0xc9])
        );
        // malformed inputs reject.
        assert_eq!(parse_mac("2:8:20:98:b1"), None); // 5 bytes
        assert_eq!(parse_mac("2:8:20:98:b1:de:ff"), None); // 7 bytes
        assert_eq!(parse_mac("2:8:20:98:b1:zz"), None); // non-hex
        assert_eq!(parse_mac("2:8:20:98:b1:123"), None); // >2 chars
        assert_eq!(parse_mac(""), None);
    }
}
