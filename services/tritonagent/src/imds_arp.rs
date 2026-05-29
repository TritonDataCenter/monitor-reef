// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Static ARP entries on `proteusimds0` for every per-port
//! pseudo-source.
//!
//! Why this exists: the tritonagent IMDS listener replies with
//! `src=169.254.169.253 dst=<pseudo_src>` (pseudo_src ∈
//! `100.64.0.0/16`). The host IP stack consults its route table,
//! sees the static `route add -net 100.64.0.0/16 169.254.169.253
//! -interface` entry installed by `proteus-kmod-load.sh`, and
//! emits the packet on `proteusimds0`. Before doing so it has to
//! resolve a dst MAC via ARP. Without a static entry the kernel
//! ARP-probes 100.64.x.y on the link, nobody answers, the packet
//! sits in the ARP queue and dies — the guest's curl hangs.
//!
//! The kmod's `INTERNAL_PORT_ID` branch in `proteus_mc_tx_opte`
//! doesn't care about the dst MAC (it demuxes by inner IPv4 dst
//! against each port's `imds_pseudo_src_v4`), so the value of the
//! ARP entry's MAC is arbitrary; we use the IMDS internal port's
//! own MAC for clarity.

use std::net::IpAddr;
use std::process::Command;

/// MAC the kmod assigns to `proteusimds0`'s GLDv3 client (mirrors
/// `proteus_api::imds::INTERNAL_PORT_MAC`). The value is
/// proteus-stable; bumping it requires a coordinated change with
/// the kmod.
const INTERNAL_PORT_MAC: &str = "02:00:00:00:00:01";

/// Add a permanent static ARP entry for `pseudo_src`. Best-effort:
/// logs a warning on failure rather than failing the binding insert
/// — the binding registry is the source of truth, ARP recovery can
/// be retried on the next provision / restart.
pub fn add(pseudo_src: IpAddr) {
    if cfg!(not(target_os = "illumos")) {
        return;
    }
    let ip = pseudo_src.to_string();
    // `arp -s <addr> <mac>` (no `temp`) installs a permanent entry
    // that survives until `arp -d` or a reboot. With `temp` the
    // entry expires on the default ARP timer (~5 min) and the
    // listener replies start failing again.
    let output = Command::new("arp")
        .args(["-s", &ip, INTERNAL_PORT_MAC])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            tracing::info!(pseudo_src = %ip, "imds: static ARP added");
        }
        Ok(out) => {
            tracing::warn!(
                pseudo_src = %ip,
                status = ?out.status,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "imds: arp -s failed"
            );
        }
        Err(e) => {
            tracing::warn!(
                pseudo_src = %ip,
                error = %e,
                "imds: arp -s could not run"
            );
        }
    }
}

/// Drop the static ARP entry for `pseudo_src`. Best-effort; a
/// missing entry isn't a failure (the binding may have come from
/// the persistence file on a fresh boot).
pub fn del(pseudo_src: IpAddr) {
    if cfg!(not(target_os = "illumos")) {
        return;
    }
    let ip = pseudo_src.to_string();
    let _ = Command::new("arp").args(["-d", &ip]).output();
}

/// Number of gratuitous-ARP probes fired on a FIP claim, and the
/// spacing between them. Three over a second is the conventional
/// burst: enough that an upstream switch/router refreshes its
/// ARP/forwarding table for the FIP's new owning CN even if one frame
/// is lost, without flooding.
const GARP_BURST: u32 = 3;
const GARP_INTERVAL_MS: u64 = 500;

/// Fire a gratuitous-ARP burst for a freshly-claimed FIP so the
/// upstream L2 re-points the FIP address at this CN's external NIC
/// (C-4b `FipClaim`, final step). Best-effort: a GARP failure is
/// logged, never fatal — the ipadm `<fip>/32` alias already answers
/// solicited ARP, so the burst only accelerates convergence after a
/// cross-CN move. illumos-gated; a no-op elsewhere.
///
/// Implemented as a temporary `arp -s` (publish) on the FIP address
/// followed by `arp -d`: illumos has no first-class "send gratuitous
/// ARP" CLI, but publishing then withdrawing the entry makes the
/// kernel emit an ARP announcement for the address on the link. The
/// permanent answer continues to come from the ipadm alias.
pub fn send_garp(fip: IpAddr) {
    if cfg!(not(target_os = "illumos")) {
        return;
    }
    let ip = fip.to_string();
    for probe in 0..GARP_BURST {
        // `arp -s <fip> <our-mac> pub` publishes a proxy entry; the
        // kernel announces it on the link. We immediately withdraw it
        // so the ipadm /32 alias remains the sole authority. The MAC
        // is irrelevant to the announcement's effect (the switch keys
        // on the source MAC of the frame, which is the external NIC's
        // own MAC); we reuse a stable placeholder.
        let pub_out = Command::new("arp")
            .args(["-s", &ip, INTERNAL_PORT_MAC, "pub"])
            .output();
        match pub_out {
            Ok(out) if out.status.success() => {
                tracing::info!(fip = %ip, probe, "fip: gratuitous ARP announced");
            }
            Ok(out) => {
                tracing::warn!(
                    fip = %ip,
                    probe,
                    status = ?out.status,
                    stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                    "fip: gratuitous arp -s pub failed (best-effort)"
                );
            }
            Err(e) => {
                tracing::warn!(fip = %ip, probe, error = %e, "fip: arp could not run");
            }
        }
        // Withdraw the published proxy entry so the ipadm alias stays
        // authoritative for solicited ARP.
        let _ = Command::new("arp").args(["-d", &ip]).output();
        if probe + 1 < GARP_BURST {
            std::thread::sleep(std::time::Duration::from_millis(GARP_INTERVAL_MS));
        }
    }
}
