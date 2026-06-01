// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Host-side network plumbing for a CN-terminated floating IP.
//!
//! On a `FipClaim` the agent adds an `<fip>/32` static address on the
//! external link so the host IP stack answers solicited ARP for the
//! FIP (the proteus inbound siphon intercepts the data frames; the
//! alias only services ARP/neighbor discovery). On `FipRelease` the
//! alias is removed. The alias name is derived deterministically from
//! the FIP address so create/delete are idempotent across agent
//! restarts and so a delete never has to guess the name.

use std::net::IpAddr;
use std::process::Command;

/// Build the ipadm address object name for a FIP alias on `link`.
/// Deterministic so `FipRelease` can reconstruct it without state:
/// `<link>/fip<addr-stripped-of-separators>`. ipadm address-object
/// names follow datalink naming (a leading letter then ASCII
/// alphanumerics only) — `.`, `:`, and `_` are all rejected with
/// "Invalid argument", so we DROP the separators rather than map them.
pub fn addr_object_name(link: &str, fip: IpAddr) -> String {
    let flat: String = fip
        .to_string()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("{link}/fip{flat}")
}

/// The `/32` (v4) or `/128` (v6) host prefix for a FIP alias.
fn host_prefix(fip: IpAddr) -> u8 {
    match fip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

/// `ipadm create-addr -t -T static -a <fip>/<prefix> <link>/fip_<addr>`
/// argument vector. Split out for unit testing the exact construction
/// (the shell-out itself is illumos-gated). `-t` makes the alias
/// temporary (not persisted across reboot): the FIP set is reconciled
/// from tritond's desired state on agent re-register, so a persistent
/// alias would only risk a stale leftover after a cross-CN move.
pub fn create_addr_args(link: &str, fip: IpAddr) -> Vec<String> {
    let object = addr_object_name(link, fip);
    let addr = format!("{}/{}", fip, host_prefix(fip));
    vec![
        "create-addr".to_string(),
        "-t".to_string(),
        "-T".to_string(),
        "static".to_string(),
        "-a".to_string(),
        addr,
        object,
    ]
}

/// `ipadm delete-addr <link>/fip_<addr>` argument vector.
pub fn delete_addr_args(link: &str, fip: IpAddr) -> Vec<String> {
    vec!["delete-addr".to_string(), addr_object_name(link, fip)]
}

/// `ipadm create-if -t <link>` argument vector — the temporary IP
/// interface a FIP `/32` alias is created on.
pub fn create_if_args(link: &str) -> Vec<String> {
    vec!["create-if".to_string(), "-t".to_string(), link.to_string()]
}

/// Ensure a temporary IP interface exists on `link` so an address can
/// be created on it. Idempotent: an already-plumbed interface is
/// success (the agent re-claims at re-register).
fn ensure_ip_interface(link: &str) -> anyhow::Result<()> {
    let out = Command::new("/usr/sbin/ipadm")
        .args(create_if_args(link))
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("already exists") || stderr.contains("Interface already in use") {
        return Ok(());
    }
    anyhow::bail!("ipadm create-if {link} failed: {}", stderr.trim());
}

/// Add the `<fip>/32` alias on `link`. Idempotent best-effort: an
/// "object exists" failure on re-claim is treated as success (the
/// alias is already there). Other failures are returned so the claim
/// fails and the saga can retry.
pub fn create_addr(link: &str, fip: IpAddr) -> anyhow::Result<()> {
    if cfg!(not(target_os = "illumos")) {
        return Ok(());
    }
    // `ipadm create-addr` requires an IP interface on the link first;
    // without it the kernel rejects the address with "Invalid argument".
    // The interface is temporary (`-t`), matching the alias, since the
    // FIP set is reconciled from tritond's desired state on re-register.
    ensure_ip_interface(link)?;
    let args = create_addr_args(link, fip);
    let out = Command::new("/usr/sbin/ipadm").args(&args).output()?;
    if out.status.success() {
        tracing::info!(%fip, link, "fip: ipadm alias created");
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // ipadm reports an existing address object as already-in-use; a
    // re-claim at a higher generation must be a no-op here.
    if stderr.contains("already exists") || stderr.contains("Object already in use") {
        tracing::info!(%fip, link, "fip: ipadm alias already present (idempotent)");
        return Ok(());
    }
    anyhow::bail!("ipadm create-addr {} on {link} failed: {}", fip, stderr.trim());
}

/// Remove the `<fip>/32` alias from `link`. Idempotent best-effort: a
/// missing object is success (release is replay-safe).
pub fn delete_addr(link: &str, fip: IpAddr) {
    if cfg!(not(target_os = "illumos")) {
        return;
    }
    let args = delete_addr_args(link, fip);
    let out = Command::new("/usr/sbin/ipadm").args(&args).output();
    match out {
        Ok(o) if o.status.success() => {
            tracing::info!(%fip, link, "fip: ipadm alias deleted");
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stderr.contains("not found") || stderr.contains("Object not found") {
                tracing::info!(%fip, link, "fip: ipadm alias already gone (idempotent)");
            } else {
                tracing::warn!(
                    %fip, link,
                    stderr = %stderr.trim(),
                    "fip: ipadm delete-addr failed (best-effort)"
                );
            }
        }
        Err(e) => {
            tracing::warn!(%fip, link, error = %e, "fip: ipadm could not run");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn addr_object_name_strips_separators_v4_and_v6() {
        // ipadm rejects `.`/`:`/`_` in addr-object names; the separators
        // are dropped, leaving a leading-letter alphanumeric name.
        let v4 = addr_object_name("external0", IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)));
        assert_eq!(v4, "external0/fip1920210");
        let v6 = addr_object_name(
            "external0",
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
        );
        assert_eq!(v6, "external0/fip2001db81");
        // No underscores/dots/colons survive (the ipadm "Invalid
        // argument" trigger).
        assert!(!v4.split('/').nth(1).unwrap().contains(['_', '.', ':']));
    }

    #[test]
    fn create_addr_args_v4_uses_slash_32() {
        let args = create_addr_args("external0", IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)));
        assert_eq!(
            args,
            vec![
                "create-addr",
                "-t",
                "-T",
                "static",
                "-a",
                "192.0.2.10/32",
                "external0/fip1920210",
            ]
        );
    }

    #[test]
    fn create_addr_args_v6_uses_slash_128() {
        let fip = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
        let args = create_addr_args("external0", fip);
        assert_eq!(args[5], "2001:db8::1/128");
    }

    #[test]
    fn delete_addr_args_reconstructs_object_name() {
        let args = delete_addr_args("external0", IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)));
        assert_eq!(args, vec!["delete-addr", "external0/fip1920210"]);
    }
}
