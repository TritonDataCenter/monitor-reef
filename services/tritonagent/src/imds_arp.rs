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
