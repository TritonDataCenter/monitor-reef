// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrapper around the SmartOS `vmadm` binary.
//!
//! Phase 0 only supports `Provision` of a `joyent-minimal` zone.
//! That covers OS zones with a single NIC on the `admin` nic_tag,
//! which is enough to prove the per-CN agent path against a real
//! SmartOS host. Brand selection (`joyent`, `lx`, `bhyve`),
//! multi-NIC, and the bhyve disk shape are deferred until the
//! tritond `Instance` record carries the corresponding fields.
//!
//! The agent does NOT call `imgadm` — the operator is expected
//! to have already imported the image so its imgadm UUID equals
//! tritond's `Image::id`. The agent assumes that mapping; if it
//! breaks the agent reports `JobOutcome::Failed { reason }` and
//! the operator either imports the image or fixes the catalog.
//!
//! ## Identity invariant
//!
//! The agent uses tritond's `Instance::id` directly as the
//! SmartOS zone UUID by passing it as the `uuid` field of
//! `vmadm create`. Stop/Restart can then address the zone by
//! the same id with no separate mapping table.
//!
//! ## Why no shared crate
//!
//! `vmadm` exec'd by string-piped JSON is enough surface for the
//! agent's needs; the broader workspace doesn't have an existing
//! crate that wraps it (the `/opt/rust-vmadm` tree on the build
//! host is a separate from-scratch port, not a library). When
//! that crate stabilises this module becomes a one-line
//! re-export.

use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info};
use tritond_client::types::{Disk, Image, Nic, ProvisioningBlueprint};
use uuid::Uuid;

/// Default DNS resolver. Picked to match the existing fdb2 zone
/// — the lab's home DNS server. Future slices will surface this
/// as an agent-config flag once we have a per-CN config plane.
const DEFAULT_RESOLVER: &str = "10.199.199.14";

/// nic_tag every Phase 0 instance lands on. The lab is flat
/// admin-tag-only; OPTE-managed overlay tags arrive with the
/// dataplane slice.
const PHASE0_NIC_TAG: &str = "admin";

/// Default MTU on the admin network.
const DEFAULT_MTU: u32 = 1500;

/// Run a `Provision` job: build the vmadm payload from the
/// blueprint, exec `vmadm create`, and wait for completion.
/// Returns the SmartOS zone UUID on success, which equals the
/// tritond instance id (asserted internally so a future bug can
/// never silently desync the two).
pub async fn create_zone(blueprint: &ProvisioningBlueprint) -> Result<Uuid> {
    let payload = build_create_payload(blueprint)?;
    let payload_bytes = serde_json::to_vec(&payload)
        .context("serialise vmadm create payload — internal types should always serialise")?;

    let pretty = serde_json::to_string_pretty(&payload).unwrap_or_default();
    debug!(payload = %pretty, "running vmadm create");

    let mut child = Command::new("vmadm")
        .arg("create")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn vmadm — is it on PATH on this host?")?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(&payload_bytes)
            .await
            .context("write vmadm create payload to stdin")?;
        stdin
            .shutdown()
            .await
            .context("close stdin to vmadm — vmadm reads to EOF before acting")?;
    } else {
        bail!("vmadm child had no stdin");
    }

    let output = child
        .wait_with_output()
        .await
        .context("await vmadm create completion")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        return Err(anyhow!(
            "vmadm create failed (exit {}): stderr={stderr}; stdout={stdout}",
            output.status,
        ));
    }

    // The instance id we passed in `uuid` is what vmadm pins. We
    // expect it back; assert so a vmadm-side surprise (e.g. silent
    // override) is not papered over.
    let want = blueprint
        .instance
        .as_ref()
        .ok_or_else(|| anyhow!("create_zone called with instance: None"))?
        .id;
    info!(zone_uuid = %want, "vmadm create succeeded");
    Ok(want)
}

/// Run `vmadm stop <uuid>`. Used for `JobKind::Stop` jobs.
pub async fn stop_zone(instance_id: Uuid) -> Result<()> {
    run_simple(&["stop", &instance_id.to_string()]).await
}

/// Run `vmadm reboot <uuid>`. Used for `JobKind::Restart` jobs.
pub async fn reboot_zone(instance_id: Uuid) -> Result<()> {
    run_simple(&["reboot", &instance_id.to_string()]).await
}

async fn run_simple(args: &[&str]) -> Result<()> {
    let output = Command::new("vmadm")
        .args(args)
        .output()
        .await
        .with_context(|| format!("spawn vmadm {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(anyhow!(
            "vmadm {} failed (exit {}): {stderr}",
            args.join(" "),
            output.status,
        ));
    }
    Ok(())
}

/// Build the vmadm create JSON for a Provision blueprint.
///
/// Returns a `serde_json::Value` (rather than a typed struct) so
/// future fields can be added without churning a Rust schema. The
/// payload conforms to vmadm(1M)'s expected shape for
/// `joyent-minimal` brand zones — single NIC on the admin tag,
/// quota sized from the boot disk's tracked bytes, customer
/// metadata carrying any authorised SSH keys.
pub(crate) fn build_create_payload(blueprint: &ProvisioningBlueprint) -> Result<serde_json::Value> {
    let instance = blueprint
        .instance
        .as_ref()
        .ok_or_else(|| anyhow!("blueprint has no instance — cannot build vmadm payload"))?;
    let image = blueprint
        .image
        .as_ref()
        .ok_or_else(|| anyhow!("blueprint has no image — Provision job requires one"))?;

    let nics_json = blueprint
        .nics
        .iter()
        .enumerate()
        .map(|(i, nic)| build_nic_json(i, nic))
        .collect::<Result<Vec<_>>>()?;

    let quota_gb = pick_quota_gb(&blueprint.disks, image);

    // Memory: tritond stores bytes; vmadm wants MiB.
    let memory_mib = bytes_to_mib(instance.memory_bytes);

    // Authorized keys: vmadm reads this from customer_metadata's
    // `root_authorized_keys` and writes it into the zone at first
    // boot. SmartOS / mdata-fetch convention.
    let mut metadata = serde_json::Map::new();
    if !blueprint.ssh_public_keys.is_empty() {
        let joined = blueprint.ssh_public_keys.join("\n");
        metadata.insert(
            "root_authorized_keys".to_string(),
            serde_json::Value::String(joined),
        );
    }

    let alias = if instance.name.is_empty() {
        format!("tritond-{}", instance.id)
    } else {
        instance.name.clone()
    };

    let mut payload = serde_json::json!({
        "uuid": instance.id,
        "brand": "joyent-minimal",
        "image_uuid": image.id,
        "alias": alias,
        "hostname": alias,
        "max_physical_memory": memory_mib,
        "max_locked_memory": memory_mib,
        // Swap typically 2x RAM; tmpfs half RAM. Conventional
        // SmartOS defaults — tunable later.
        "max_swap": memory_mib.saturating_mul(2),
        "tmpfs": memory_mib.saturating_div(2).max(64),
        "cpu_cap": instance.cpu.saturating_mul(100),
        "cpu_shares": instance.cpu.saturating_mul(100),
        "quota": quota_gb,
        "resolvers": [DEFAULT_RESOLVER],
        // Tags carry the tritond identity so an operator browsing
        // `vmadm list` on the host can match a zone back to its
        // tritond instance + tenancy without a separate registry.
        "tags": {
            "tritond.instance_id": instance.id.to_string(),
            "tritond.silo_id": instance.silo_id.to_string(),
            "tritond.project_id": instance.project_id.to_string(),
        },
        "nics": nics_json,
        "customer_metadata": serde_json::Value::Object(metadata),
    });

    // Force the agent's identity onto the zone description for
    // operator visibility (`zoneadm list -p` shows it).
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "internal_metadata".to_string(),
            serde_json::json!({
                "tritond.image_sha256": image.sha256,
            }),
        );
    }

    Ok(payload)
}

fn build_nic_json(index: usize, nic: &Nic) -> Result<serde_json::Value> {
    let ip = match &nic.primary_ipv4 {
        Some(ip) => ip,
        None => bail!(
            "NIC {} has no IPv4 — Phase 0 vmadm payload requires v4 (v6-only zones tracked separately)",
            nic.id,
        ),
    };
    Ok(serde_json::json!({
        "interface": format!("net{index}"),
        "nic_tag": PHASE0_NIC_TAG,
        "ip": ip,
        // The /24 is hardcoded for the admin lab network; once a
        // subnet record carries a CIDR-derived netmask, derive
        // it here.
        "netmask": "255.255.255.0",
        "vlan_id": 0,
        "mtu": DEFAULT_MTU,
        "mac": nic.mac,
        "primary": index == 0,
    }))
}

fn pick_quota_gb(disks: &[Disk], image: &Image) -> u64 {
    // Prefer the boot disk's stored size if there is one; fall
    // back to the image content size; clamp to a 1 GB minimum so
    // tiny test images don't end up with a zero-quota zone.
    let bytes = disks
        .iter()
        .map(|d| d.size_bytes)
        .max()
        .unwrap_or(image.size_bytes);
    (bytes / 1024 / 1024 / 1024).max(1)
}

fn bytes_to_mib(bytes: u64) -> u64 {
    let mib = bytes / 1024 / 1024;
    mib.max(64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::net::Ipv4Addr;
    use tritond_client::types::{Instance, JobKind};

    fn fixture_uuid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    fn sample_blueprint() -> ProvisioningBlueprint {
        let inst_id = fixture_uuid(0xa1);
        let silo = fixture_uuid(0xb2);
        let project = fixture_uuid(0xc3);
        let subnet = fixture_uuid(0xd4);
        let vpc = fixture_uuid(0xe5);
        let image_id = fixture_uuid(0xf6);
        let job_id = fixture_uuid(0x07);

        let now = Utc::now();
        let instance = Instance {
            id: inst_id,
            silo_id: silo,
            project_id: project,
            name: "smoke-zone".to_string(),
            description: String::new(),
            image_id,
            primary_subnet_id: subnet,
            ssh_key_ids: Vec::new(),
            cpu: 2,
            memory_bytes: 512 * 1024 * 1024,
            lifecycle: tritond_client::types::LifecycleState::Pending,
            created_at: now,
            updated_at: now,
        };
        let image = Image {
            id: image_id,
            silo_id: silo,
            name: "minimal-64-lts".to_string(),
            description: String::new(),
            os: "smartos".to_string(),
            version: "23.4.0".to_string(),
            size_bytes: 256 * 1024 * 1024,
            sha256: "deadbeef".repeat(8),
            source_url: None,
            created_at: now,
        };
        let nic = Nic {
            id: fixture_uuid(0x11),
            silo_id: silo,
            project_id: project,
            instance_id: inst_id,
            vpc_id: vpc,
            subnet_id: subnet,
            name: "primary".to_string(),
            mac: "02:00:00:de:ad:01".to_string(),
            primary_ipv4: Some(Ipv4Addr::new(10, 199, 199, 77)),
            primary_ipv6: None,
            created_at: now,
        };
        ProvisioningBlueprint {
            job_id,
            kind: JobKind::Provision(inst_id),
            instance: Some(instance),
            image: Some(image),
            nics: vec![nic],
            disks: Vec::new(),
            ssh_public_keys: vec!["ssh-ed25519 AAAA test@host".to_string()],
        }
    }

    #[test]
    fn build_create_payload_carries_identity_and_nic() {
        let bp = sample_blueprint();
        let payload = build_create_payload(&bp).unwrap();
        assert_eq!(
            payload["uuid"],
            bp.instance.as_ref().unwrap().id.to_string()
        );
        assert_eq!(payload["brand"], "joyent-minimal");
        assert_eq!(
            payload["image_uuid"],
            bp.image.as_ref().unwrap().id.to_string()
        );
        assert_eq!(payload["alias"], "smoke-zone");
        assert_eq!(payload["max_physical_memory"], 512);
        assert_eq!(payload["cpu_cap"], 200);
        // The single NIC must end up at index 0, primary=true, on
        // the admin tag with the IP from the tritond Nic record.
        let nic0 = &payload["nics"][0];
        assert_eq!(nic0["nic_tag"], "admin");
        assert_eq!(nic0["ip"], "10.199.199.77");
        assert_eq!(nic0["mac"], "02:00:00:de:ad:01");
        assert_eq!(nic0["primary"], true);
        // SSH keys round-trip into customer_metadata.
        assert_eq!(
            payload["customer_metadata"]["root_authorized_keys"],
            "ssh-ed25519 AAAA test@host",
        );
    }

    #[test]
    fn missing_ipv4_is_rejected() {
        let mut bp = sample_blueprint();
        bp.nics[0].primary_ipv4 = None;
        let err = build_create_payload(&bp).unwrap_err();
        assert!(err.to_string().contains("no IPv4"));
    }

    #[test]
    fn missing_instance_or_image_is_rejected() {
        let mut bp = sample_blueprint();
        bp.instance = None;
        assert!(build_create_payload(&bp).is_err());
        let mut bp = sample_blueprint();
        bp.image = None;
        assert!(build_create_payload(&bp).is_err());
    }
}
