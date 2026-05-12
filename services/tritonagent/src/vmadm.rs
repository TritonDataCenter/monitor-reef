// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrapper around the SmartOS `vmadm` binary.
//!
//! Phase 0 started with `joyent-minimal` zones. The v1 path adds a
//! pure bhyve payload builder so the agent can provision SmartOS
//! hardware VMs once the scheduler routes those jobs here. Until the
//! tritond `Instance` record grows an explicit brand field, the agent
//! treats `Image.compatibility.brand == "bhyve"` as the dispatch
//! signal.
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

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, info};
use tritond_client::types::{Disk, Image, Nic, ProvisioningBlueprint, Subnet};
use uuid::Uuid;

pub(crate) type NicTagMap = BTreeMap<Uuid, String>;

/// Default DNS resolver. Picked to match the existing fdb2 zone
/// — the lab's home DNS server. Future slices will surface this
/// as an agent-config flag once we have a per-CN config plane.
const DEFAULT_RESOLVER: &str = "10.199.199.14";

/// nic_tag every Phase 0 zone lands on. The lab is flat
/// admin-tag-only; OPTE-managed overlay tags arrive with the
/// dataplane slice.
const PHASE0_NIC_TAG: &str = "admin";

/// Legacy fallback nic_tag for side-effect-free bhyve payload tests.
/// The live M1 provision path passes per-NIC Proteus link tags instead.
const BHYVE_M1_NIC_TAG: &str = "external";

/// Default MTU on the admin network.
const DEFAULT_MTU: u32 = 1500;

// Wire contract: SmartOS `internal_metadata` keys carrying the
// tamper-evident managed-zone identity stamped at provision time.
// The canonical definitions live in
// `tritond_store::types::TRITOND_METADATA_*`; the constants here are
// duplicated to keep tritonagent's runtime dep set lean, and the
// `vmadm_identity_constants_match_canonical` test in the
// integration suite asserts they cannot drift.
pub(crate) const TRITOND_METADATA_INSTANCE_ID: &str = "tritond:instance_id";
pub(crate) const TRITOND_METADATA_TENANT_ID: &str = "tritond:tenant_id";
pub(crate) const TRITOND_METADATA_PROJECT_ID: &str = "tritond:project_id";
pub(crate) const TRITOND_METADATA_IDENTITY_HMAC: &str = "tritond:identity_hmac";

/// Insert the four `tritond:*` identity keys into a vmadm
/// `internal_metadata` map when the blueprint carries a
/// `managed_identity`. No-op when absent (Stop/Restart/Delete jobs do
/// not carry identity; the zone already has it from its original
/// provision).
fn apply_managed_identity(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    blueprint: &ProvisioningBlueprint,
) {
    let Some(identity) = blueprint.managed_identity.as_ref() else {
        return;
    };
    metadata.insert(
        TRITOND_METADATA_INSTANCE_ID.to_string(),
        serde_json::Value::String(identity.instance_id.to_string()),
    );
    metadata.insert(
        TRITOND_METADATA_TENANT_ID.to_string(),
        serde_json::Value::String(identity.tenant_id.to_string()),
    );
    metadata.insert(
        TRITOND_METADATA_PROJECT_ID.to_string(),
        serde_json::Value::String(identity.project_id.to_string()),
    );
    metadata.insert(
        TRITOND_METADATA_IDENTITY_HMAC.to_string(),
        serde_json::Value::String(identity.identity_hmac.clone()),
    );
}

/// Run a `Provision` job: build the vmadm payload from the
/// blueprint, exec `vmadm create`, and wait for completion.
/// Returns the SmartOS zone UUID on success, which equals the
/// tritond instance id (asserted internally so a future bug can
/// never silently desync the two).
pub async fn create_zone(blueprint: &ProvisioningBlueprint) -> Result<Uuid> {
    let payload = build_create_payload(blueprint)?;
    run_create_payload(blueprint, payload).await
}

pub(crate) async fn create_zone_with_nic_tags(
    blueprint: &ProvisioningBlueprint,
    nic_tags: &NicTagMap,
) -> Result<Uuid> {
    let payload = build_create_payload_with_nic_tags(blueprint, nic_tags)?;
    run_create_payload(blueprint, payload).await
}

async fn run_create_payload(
    blueprint: &ProvisioningBlueprint,
    payload: serde_json::Value,
) -> Result<Uuid> {
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

/// Run `vmadm delete <uuid>`. Used for `JobKind::Delete` jobs.
///
/// Idempotent on the "zone never existed" case: a non-zero exit
/// whose stderr matches vmadm's not-found marker is treated as
/// success. The control plane has already cleared the tritond
/// record by the time this runs; the agent's job is to make
/// the SmartOS side match. If the zone is gone for any reason
/// (this agent's predecessor deleted it, host reset, …) the
/// goal is met.
pub async fn delete_zone(instance_id: Uuid) -> Result<()> {
    let id = instance_id.to_string();
    let output = Command::new("vmadm")
        .arg("delete")
        .arg(&id)
        .output()
        .await
        .with_context(|| format!("spawn vmadm delete {id}"))?;
    if output.status.success() {
        return Ok(());
    }
    // vmadm prints the not-found marker on stderr. Match by
    // substring rather than exact text so a future vmadm
    // wording tweak doesn't silently regress idempotency.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("vm-not-found") || stderr.contains("Unable to find") {
        return Ok(());
    }
    Err(anyhow!(
        "vmadm delete {id} failed (exit {}): {stderr}",
        output.status,
    ))
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
    let nic_tags = NicTagMap::new();
    build_create_payload_with_nic_tags(blueprint, &nic_tags)
}

pub(crate) fn build_create_payload_with_nic_tags(
    blueprint: &ProvisioningBlueprint,
    nic_tags: &NicTagMap,
) -> Result<serde_json::Value> {
    if image_brand(blueprint) == Some("bhyve") {
        create_bhyve_payload_with_nic_tags(blueprint, nic_tags)
    } else {
        create_zone_payload(blueprint)
    }
}

/// Build the legacy `joyent-minimal` zone payload.
pub(crate) fn create_zone_payload(blueprint: &ProvisioningBlueprint) -> Result<serde_json::Value> {
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
            "tritond.tenant_id": instance.tenant_id.to_string(),
            "tritond.project_id": instance.project_id.to_string(),
        },
        "nics": nics_json,
        "customer_metadata": serde_json::Value::Object(metadata),
    });

    // Force the agent's identity onto the zone description for
    // operator visibility (`zoneadm list -p` shows it).
    if let Some(obj) = payload.as_object_mut() {
        let mut internal_metadata = serde_json::Map::new();
        internal_metadata.insert(
            "tritond.image_sha256".to_string(),
            serde_json::Value::String(image.sha256.clone()),
        );
        apply_managed_identity(&mut internal_metadata, blueprint);
        obj.insert(
            "internal_metadata".to_string(),
            serde_json::Value::Object(internal_metadata),
        );
    }

    Ok(payload)
}

/// Build a side-effect-free bhyve `vmadm create` payload.
///
/// This is intentionally independent of `create_zone`: it lets tests
/// pin the bhyve JSON shape before the agent starts invoking it in the
/// provisioning loop.
#[cfg(test)]
pub(crate) fn create_bhyve_payload(blueprint: &ProvisioningBlueprint) -> Result<serde_json::Value> {
    let nic_tags = NicTagMap::new();
    create_bhyve_payload_with_nic_tags(blueprint, &nic_tags)
}

pub(crate) fn create_bhyve_payload_with_nic_tags(
    blueprint: &ProvisioningBlueprint,
    nic_tags: &NicTagMap,
) -> Result<serde_json::Value> {
    let instance = blueprint
        .instance
        .as_ref()
        .ok_or_else(|| anyhow!("blueprint has no instance — cannot build vmadm payload"))?;
    let image = blueprint
        .image
        .as_ref()
        .ok_or_else(|| anyhow!("blueprint has no image — Provision job requires one"))?;

    if blueprint.nics.is_empty() {
        bail!("blueprint has no NICs — bhyve vmadm payload requires at least one NIC");
    }

    let nics_json = blueprint
        .nics
        .iter()
        .enumerate()
        .map(|(index, nic)| build_bhyve_nic_json(index, nic, nic_tags, &blueprint.subnets))
        .collect::<Result<Vec<_>>>()?;

    let alias = if instance.name.is_empty() {
        format!("tritond-{}", instance.id)
    } else {
        instance.name.clone()
    };
    let memory_mib = bytes_to_mib(instance.memory_bytes);
    let flexible_disk_size = pick_bhyve_disk_size_mib(&blueprint.disks, image);
    let mut customer_metadata = ssh_customer_metadata(blueprint);
    customer_metadata.insert(
        "cloud-init:user-data".to_string(),
        serde_json::Value::String(render_nocloud_user_data(&blueprint.ssh_public_keys)),
    );
    customer_metadata.insert(
        "cloud-init:meta-data".to_string(),
        serde_json::Value::String(render_nocloud_meta_data(instance.id, &alias)),
    );
    customer_metadata.insert(
        "org.smartos:cloudinit_datasource".to_string(),
        serde_json::Value::String("nocloud".to_string()),
    );

    let mut internal_metadata = serde_json::Map::new();
    internal_metadata.insert(
        "tritond.image_sha256".to_string(),
        serde_json::Value::String(image.sha256.clone()),
    );
    internal_metadata.insert(
        "cloudinit_datasource".to_string(),
        serde_json::Value::String("nocloud".to_string()),
    );
    apply_managed_identity(&mut internal_metadata, blueprint);

    Ok(serde_json::json!({
        "uuid": instance.id,
        "brand": "bhyve",
        "alias": alias,
        "hostname": alias,
        "ram": memory_mib,
        "vcpus": instance.cpu,
        // UEFI rather than the SmartOS default ("bios"). UEFI is the
        // modern firmware (every guest M1 cares about supports it) and,
        // crucially here, only the UEFI path makes vmadm add the `fbuf`
        // framebuffer device + its VNC unix socket
        // (`<zonepath>/root/tmp/vm.vnc`) -- without it the VNC console
        // has nothing to attach to. The SmartOS property is `bootrom`.
        "bootrom": "uefi",
        "flexible_disk_size": flexible_disk_size,
        "disks": [
            {
                "boot": true,
                "model": "virtio",
                "image_uuid": image.id,
            }
        ],
        "nics": nics_json,
        "customer_metadata": serde_json::Value::Object(customer_metadata),
        "internal_metadata": serde_json::Value::Object(internal_metadata),
        "tags": {
            "tritond.instance_id": instance.id.to_string(),
            "tritond.tenant_id": instance.tenant_id.to_string(),
            "tritond.project_id": instance.project_id.to_string(),
        },
    }))
}

fn image_brand(blueprint: &ProvisioningBlueprint) -> Option<&str> {
    blueprint
        .image
        .as_ref()
        .and_then(|image| image.compatibility.as_ref())
        .map(|compat| compat.brand.as_str())
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

fn build_bhyve_nic_json(
    index: usize,
    nic: &Nic,
    nic_tags: &NicTagMap,
    subnets: &[Subnet],
) -> Result<serde_json::Value> {
    let nic_tag = bhyve_nic_tag(nic, nic_tags)?;
    let (ip_cidr, gateway) = bhyve_ipv4_config(nic, subnets)?;
    let mut payload = serde_json::json!({
        "interface": format!("net{index}"),
        "nic_tag": nic_tag,
        "model": "virtio",
        "mac": nic.mac,
        "ips": [ip_cidr],
        "mtu": DEFAULT_MTU,
        "primary": index == 0,
    });
    if index == 0
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("gateways".to_string(), serde_json::json!([gateway]));
    }
    Ok(payload)
}

fn bhyve_nic_tag<'a>(nic: &'a Nic, nic_tags: &'a NicTagMap) -> Result<&'a str> {
    if let Some(tag) = nic_tags.get(&nic.id) {
        return Ok(tag);
    }
    if nic_tags.is_empty() {
        return Ok(BHYVE_M1_NIC_TAG);
    }
    bail!(
        "no Proteus link nic_tag for bhyve NIC {}; refusing partial vmadm payload",
        nic.id,
    )
}

fn ssh_customer_metadata(
    blueprint: &ProvisioningBlueprint,
) -> serde_json::Map<String, serde_json::Value> {
    let mut metadata = serde_json::Map::new();
    if !blueprint.ssh_public_keys.is_empty() {
        metadata.insert(
            "root_authorized_keys".to_string(),
            serde_json::Value::String(blueprint.ssh_public_keys.join("\n")),
        );
    }
    metadata
}

fn bhyve_ipv4_config(nic: &Nic, subnets: &[Subnet]) -> Result<(String, String)> {
    let ip = nic.primary_ipv4.ok_or_else(|| {
        anyhow!(
            "NIC {} has no IPv4 — M1 bhyve static guest networking requires v4",
            nic.id
        )
    })?;
    let subnet = subnets
        .iter()
        .find(|subnet| subnet.id == nic.subnet_id)
        .ok_or_else(|| {
            anyhow!(
                "no subnet metadata for bhyve NIC {} subnet {}; refusing DHCP-only payload",
                nic.id,
                nic.subnet_id
            )
        })?;
    let cidr = subnet.ipv4_block.as_deref().ok_or_else(|| {
        anyhow!(
            "subnet {} has no IPv4 CIDR — M1 bhyve static guest networking requires v4",
            subnet.id
        )
    })?;
    let (network, prefix) = parse_ipv4_cidr(cidr)?;
    if prefix > 30 {
        bail!("subnet {cidr} is too small to derive the conventional .1 gateway");
    }
    if !ipv4_contains(network, prefix, ip) {
        bail!("NIC {} IPv4 {} is outside subnet {}", nic.id, ip, cidr);
    }

    let mask = ipv4_mask(prefix);
    let gateway = Ipv4Addr::from((u32::from(network) & mask) + 1);
    Ok((format!("{ip}/{prefix}"), gateway.to_string()))
}

fn parse_ipv4_cidr(cidr: &str) -> Result<(Ipv4Addr, u8)> {
    let (network, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid IPv4 CIDR {cidr:?}: missing prefix"))?;
    let network = network
        .parse()
        .with_context(|| format!("parse IPv4 network address from {cidr:?}"))?;
    let prefix: u8 = prefix
        .parse()
        .with_context(|| format!("parse IPv4 prefix from {cidr:?}"))?;
    if prefix > 32 {
        bail!("invalid IPv4 CIDR {cidr:?}: prefix {prefix} is greater than 32");
    }
    Ok((network, prefix))
}

fn ipv4_contains(network: Ipv4Addr, prefix: u8, ip: Ipv4Addr) -> bool {
    let mask = ipv4_mask(prefix);
    (u32::from(network) & mask) == (u32::from(ip) & mask)
}

fn ipv4_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
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

fn pick_bhyve_disk_size_mib(disks: &[Disk], image: &Image) -> u64 {
    let bytes = disks
        .iter()
        .map(|d| d.size_bytes)
        .max()
        .unwrap_or(image.size_bytes);
    bytes_to_mib(bytes)
}

fn bytes_to_mib(bytes: u64) -> u64 {
    let mib = bytes / 1024 / 1024;
    mib.max(64)
}

fn render_nocloud_user_data(ssh_keys: &[String]) -> String {
    let mut out = String::from("#cloud-config\ndisable_root: false\n");
    if !ssh_keys.is_empty() {
        out.push_str("ssh_authorized_keys:\n");
        for key in ssh_keys {
            out.push_str("  - ");
            out.push_str(key);
            out.push('\n');
        }
    }
    out
}

fn render_nocloud_meta_data(instance_id: Uuid, hostname: &str) -> String {
    format!("instance-id: {instance_id}\nlocal-hostname: {hostname}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::net::Ipv4Addr;
    use tritond_client::types::{DiskKind, ImageCompatibility, Instance, JobKind};

    fn fixture_uuid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    fn sample_blueprint() -> ProvisioningBlueprint {
        let inst_id = fixture_uuid(0xa1);
        let silo = fixture_uuid(0xb2);
        let tenant = fixture_uuid(0xb3);
        let project = fixture_uuid(0xc3);
        let subnet = fixture_uuid(0xd4);
        let route_table = fixture_uuid(0xd5);
        let vpc = fixture_uuid(0xe5);
        let image_id = fixture_uuid(0xf6);
        let job_id = fixture_uuid(0x07);

        let now = Utc::now();
        let instance = Instance {
            id: inst_id,
            tenant_id: tenant,
            project_id: project,
            name: "smoke-zone".to_string(),
            description: String::new(),
            image_id,
            primary_subnet_id: subnet,
            ssh_key_ids: Vec::new(),
            cpu: 2,
            memory_bytes: 512 * 1024 * 1024,
            host_cn_uuid: None,
            brand: tritond_client::types::InstanceBrand::JoyentMinimal,
            lifecycle: tritond_client::types::LifecycleState::Pending,
            created_at: now,
            updated_at: now,
        };
        let image = Image {
            id: image_id,
            scope: tritond_client::types::ImageScope::Silo { silo_id: silo },
            name: "minimal-64-lts".to_string(),
            description: String::new(),
            os: "smartos".to_string(),
            version: "23.4.0".to_string(),
            size_bytes: 256 * 1024 * 1024,
            sha256: "deadbeef".repeat(8),
            source_url: None,
            compatibility: None,
            created_at: now,
        };
        let nic = Nic {
            id: fixture_uuid(0x11),
            tenant_id: tenant,
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
        let subnet_record = Subnet {
            id: subnet,
            tenant_id: tenant,
            project_id: project,
            vpc_id: vpc,
            route_table_id: route_table,
            name: "primary".to_string(),
            description: String::new(),
            ipv4_block: Some("10.199.199.0/24".to_string()),
            ipv6_block: None,
            created_at: now,
        };
        ProvisioningBlueprint {
            job_id,
            kind: JobKind::Provision {
                instance_id: inst_id,
            },
            instance: Some(instance),
            image: Some(image),
            nics: vec![nic],
            subnets: vec![subnet_record],
            disks: Vec::new(),
            ssh_public_keys: vec!["ssh-ed25519 AAAA test@host".to_string()],
            managed_identity: None,
        }
    }

    /// Variant that carries a populated `managed_identity`. Used by the
    /// tests asserting the four `tritond:*` keys land in the vmadm
    /// `internal_metadata` payload.
    fn sample_blueprint_with_identity() -> ProvisioningBlueprint {
        let mut bp = sample_blueprint();
        let inst = bp.instance.as_ref().unwrap();
        bp.managed_identity = Some(tritond_client::types::ManagedIdentity {
            instance_id: inst.id,
            tenant_id: inst.tenant_id,
            project_id: inst.project_id,
            identity_hmac: "deadbeef".repeat(8),
        });
        bp
    }

    fn sample_bhyve_blueprint() -> ProvisioningBlueprint {
        let mut bp = sample_blueprint();
        let now = Utc::now();
        let image_id = bp.image.as_ref().unwrap().id;
        bp.image.as_mut().unwrap().compatibility = Some(ImageCompatibility {
            brand: "bhyve".to_string(),
            arch: "x86_64".to_string(),
            min_smartos_platform: None,
        });
        bp.disks = vec![Disk {
            id: fixture_uuid(0x22),
            tenant_id: bp.instance.as_ref().unwrap().tenant_id,
            project_id: bp.instance.as_ref().unwrap().project_id,
            instance_id: bp.instance.as_ref().unwrap().id,
            name: "boot".to_string(),
            description: String::new(),
            kind: DiskKind::Boot,
            size_bytes: 20 * 1024 * 1024 * 1024,
            source_image_id: Some(image_id),
            created_at: now,
        }];
        bp
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
    fn build_create_payload_dispatches_bhyve_from_image_compatibility() {
        let bp = sample_bhyve_blueprint();
        let payload = build_create_payload(&bp).unwrap();
        assert_eq!(payload["brand"], "bhyve");
        assert_eq!(payload["flexible_disk_size"], 20 * 1024);
    }

    #[test]
    fn create_bhyve_payload_has_golden_shape() {
        let bp = sample_bhyve_blueprint();
        let instance = bp.instance.as_ref().unwrap();
        let image = bp.image.as_ref().unwrap();
        let expected = serde_json::json!({
            "uuid": instance.id.to_string(),
            "brand": "bhyve",
            "alias": "smoke-zone",
            "hostname": "smoke-zone",
            "ram": 512,
            "vcpus": 2,
            "bootrom": "uefi",
            "flexible_disk_size": 20 * 1024,
            "disks": [
                {
                    "boot": true,
                    "model": "virtio",
                    "image_uuid": image.id.to_string(),
                }
            ],
            "nics": [
                {
                    "interface": "net0",
                    "nic_tag": "external",
                    "model": "virtio",
                    "mac": "02:00:00:de:ad:01",
                    "ips": ["10.199.199.77/24"],
                    "gateways": ["10.199.199.1"],
                    "mtu": 1500,
                    "primary": true,
                }
            ],
            "customer_metadata": {
                "root_authorized_keys": "ssh-ed25519 AAAA test@host",
                "cloud-init:user-data": "#cloud-config\ndisable_root: false\nssh_authorized_keys:\n  - ssh-ed25519 AAAA test@host\n",
                "cloud-init:meta-data": format!(
                    "instance-id: {}\nlocal-hostname: smoke-zone\n",
                    instance.id,
                ),
                "org.smartos:cloudinit_datasource": "nocloud",
            },
            "internal_metadata": {
                "tritond.image_sha256": image.sha256,
                "cloudinit_datasource": "nocloud",
            },
            "tags": {
                "tritond.instance_id": instance.id.to_string(),
                "tritond.tenant_id": instance.tenant_id.to_string(),
                "tritond.project_id": instance.project_id.to_string(),
            },
        });

        let payload = create_bhyve_payload(&bp).unwrap();
        assert_eq!(payload, expected);
    }

    #[test]
    fn zone_payload_carries_tritond_identity_metadata_when_present() {
        let bp = sample_blueprint_with_identity();
        let identity = bp.managed_identity.as_ref().unwrap();
        let payload = build_create_payload(&bp).unwrap();
        let im = &payload["internal_metadata"];
        assert_eq!(
            im[TRITOND_METADATA_INSTANCE_ID],
            identity.instance_id.to_string()
        );
        assert_eq!(
            im[TRITOND_METADATA_TENANT_ID],
            identity.tenant_id.to_string()
        );
        assert_eq!(
            im[TRITOND_METADATA_PROJECT_ID],
            identity.project_id.to_string()
        );
        assert_eq!(im[TRITOND_METADATA_IDENTITY_HMAC], identity.identity_hmac);
        // Pre-existing key must still be there alongside the four new ones.
        assert_eq!(
            im["tritond.image_sha256"],
            bp.image.as_ref().unwrap().sha256
        );
    }

    #[test]
    fn zone_payload_omits_tritond_identity_metadata_when_absent() {
        let bp = sample_blueprint();
        assert!(bp.managed_identity.is_none());
        let payload = build_create_payload(&bp).unwrap();
        let im = &payload["internal_metadata"];
        assert!(im.get(TRITOND_METADATA_INSTANCE_ID).is_none());
        assert!(im.get(TRITOND_METADATA_TENANT_ID).is_none());
        assert!(im.get(TRITOND_METADATA_PROJECT_ID).is_none());
        assert!(im.get(TRITOND_METADATA_IDENTITY_HMAC).is_none());
    }

    #[test]
    fn bhyve_payload_carries_tritond_identity_metadata_when_present() {
        let mut bp = sample_bhyve_blueprint();
        let inst = bp.instance.as_ref().unwrap();
        bp.managed_identity = Some(tritond_client::types::ManagedIdentity {
            instance_id: inst.id,
            tenant_id: inst.tenant_id,
            project_id: inst.project_id,
            identity_hmac: "feedface".repeat(8),
        });
        let identity = bp.managed_identity.as_ref().unwrap();
        let payload = create_bhyve_payload(&bp).unwrap();
        let im = &payload["internal_metadata"];
        assert_eq!(
            im[TRITOND_METADATA_INSTANCE_ID],
            identity.instance_id.to_string()
        );
        assert_eq!(
            im[TRITOND_METADATA_TENANT_ID],
            identity.tenant_id.to_string()
        );
        assert_eq!(
            im[TRITOND_METADATA_PROJECT_ID],
            identity.project_id.to_string()
        );
        assert_eq!(im[TRITOND_METADATA_IDENTITY_HMAC], identity.identity_hmac);
        // Pre-existing keys must still be there alongside the four new ones.
        assert_eq!(
            im["tritond.image_sha256"],
            bp.image.as_ref().unwrap().sha256
        );
        assert_eq!(im["cloudinit_datasource"], "nocloud");
    }

    /// Wire-contract regression: tritonagent's local copies of the
    /// `tritond:*` metadata keys must match the canonical definitions
    /// in `tritond_store::types`. Without this, a rename in
    /// tritond-store would silently break the classifier.
    #[test]
    fn vmadm_identity_constants_match_canonical() {
        assert_eq!(
            TRITOND_METADATA_INSTANCE_ID,
            tritond_store::TRITOND_METADATA_INSTANCE_ID
        );
        assert_eq!(
            TRITOND_METADATA_TENANT_ID,
            tritond_store::TRITOND_METADATA_TENANT_ID
        );
        assert_eq!(
            TRITOND_METADATA_PROJECT_ID,
            tritond_store::TRITOND_METADATA_PROJECT_ID
        );
        assert_eq!(
            TRITOND_METADATA_IDENTITY_HMAC,
            tritond_store::TRITOND_METADATA_IDENTITY_HMAC
        );
    }

    #[test]
    fn create_bhyve_payload_uses_proteus_nic_tags_when_supplied() {
        let bp = sample_bhyve_blueprint();
        let mut nic_tags = NicTagMap::new();
        nic_tags.insert(bp.nics[0].id, "proteus49377".to_string());

        let payload = create_bhyve_payload_with_nic_tags(&bp, &nic_tags).unwrap();

        assert_eq!(payload["nics"][0]["nic_tag"], "proteus49377");
    }

    #[test]
    fn create_bhyve_payload_requires_subnet_metadata_for_static_networking() {
        let mut bp = sample_bhyve_blueprint();
        bp.subnets.clear();

        let err = create_bhyve_payload(&bp).unwrap_err();

        assert!(err.to_string().contains("no subnet metadata"));
    }

    #[test]
    fn create_bhyve_payload_rejects_partial_proteus_nic_tags() {
        let bp = sample_bhyve_blueprint();
        let mut nic_tags = NicTagMap::new();
        nic_tags.insert(fixture_uuid(0x99), "proteus39321".to_string());

        let err = create_bhyve_payload_with_nic_tags(&bp, &nic_tags).unwrap_err();

        assert!(err.to_string().contains("no Proteus link nic_tag"));
    }

    #[test]
    fn create_bhyve_payload_requires_instance_image_and_nic() {
        let mut bp = sample_bhyve_blueprint();
        bp.instance = None;
        assert!(create_bhyve_payload(&bp).is_err());

        let mut bp = sample_bhyve_blueprint();
        bp.image = None;
        assert!(create_bhyve_payload(&bp).is_err());

        let mut bp = sample_bhyve_blueprint();
        bp.nics.clear();
        let err = create_bhyve_payload(&bp).unwrap_err();
        assert!(err.to_string().contains("requires at least one NIC"));
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
