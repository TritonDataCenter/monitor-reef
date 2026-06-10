// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrapper around the SmartOS `vmadm` binary.
//!
//! Brand dispatch reads `Image.compatibility.brand == "bhyve"`; the
//! agent uses tritond's `Instance::id` as the SmartOS zone UUID so
//! Stop/Restart can address the zone with no mapping table. The
//! agent does NOT call `imgadm` — the operator imports the image
//! to match tritond's `Image::id` or the job fails with a reason.

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

const DEFAULT_RESOLVER: &str = "10.199.199.14";

const PHASE0_NIC_TAG: &str = "admin";

/// Fallback for bhyve payload tests; live provision uses per-NIC
/// Proteus link tags instead.
const BHYVE_M1_NIC_TAG: &str = "external";

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

/// Fold `blueprint.provision_metadata` (the operator-set
/// `triton/instance/*` entries) into the vmadm create payload's
/// metadata maps:
///   * `guest_visible=true`  -> `customer_metadata.<suffix>`
///     (cloud-init's SmartOS / NoCloud datasource picks these up at
///     first boot)
///   * `guest_visible=false` -> `internal_metadata.<suffix>`
///     (the legacy "internal_metadata" shape, where the historical
///     `root_pw` lives)
/// The `instance/` prefix is stripped when folding, so a stored key
/// `instance/root_pw` (`guest_visible=false`) ends up as
/// `internal_metadata.root_pw` in the payload -- matching what
/// cloud-init / SmartOS / `mdata-get` expect.
///
/// Each value is rendered to a SmartOS-style string (the two metadata
/// maps both accept only string values): JSON strings pass through
/// unwrapped, numbers / bools stringify, structured values
/// (objects/arrays) get JSON-encoded so they at least round-trip.
fn apply_provision_metadata(
    customer_metadata: &mut serde_json::Map<String, serde_json::Value>,
    internal_metadata: &mut serde_json::Map<String, serde_json::Value>,
    blueprint: &ProvisioningBlueprint,
) {
    for entry in &blueprint.provision_metadata {
        // The store key is namespaced like `instance/<suffix>`;
        // anything else would have been filtered out tritond-side
        // but defend in depth.
        let Some(suffix) = entry.key.strip_prefix("instance/") else {
            continue;
        };
        if suffix.is_empty() {
            continue;
        }
        // Generated client flattens MetaValue into MetaEntry, so the
        // flags + value are top-level on `entry`.
        let rendered = render_meta_value_as_string(&entry.value);
        let target = if entry.guest_visible {
            &mut *customer_metadata
        } else {
            &mut *internal_metadata
        };
        target.insert(suffix.to_string(), serde_json::Value::String(rendered));
    }
}

/// Render a metadata `serde_json::Value` for one of vmadm's
/// string-only metadata maps. Strings unwrap to their inner text so
/// `"root_pw":"Nic^..."` doesn't show up doubly-quoted; everything
/// else is JSON-stringified so structured values still round-trip.
fn render_meta_value_as_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

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
    use_reservoir: bool,
) -> Result<Uuid> {
    let payload = build_create_payload_with_nic_tags(blueprint, nic_tags, use_reservoir)?;
    run_create_payload(blueprint, payload).await
}

/// Create a migration-target zone shell: the zone config and
/// dataset skeleton only, never a running guest. Used by
/// `JobKind::MigrationProvisionTarget`.
pub(crate) async fn create_migration_target_zone(
    blueprint: &ProvisioningBlueprint,
    nic_tags: &NicTagMap,
) -> Result<Uuid> {
    let payload = build_migration_target_payload(blueprint, nic_tags)?;
    run_create_payload(blueprint, payload).await
}

/// Build the `vmadm create` payload for a migration target.
///
/// Two deltas against a normal provision:
///   * `autoboot: false`: booting before the cutover would put a
///     duplicate-MAC guest on the wire while the source still
///     owns the identity.
///   * bhyve disks lose their `image_uuid`: the vmadm-created
///     datasets are destroyed right after create so the first
///     `zfs recv` lands clean, so cloning a (possibly absent)
///     image into them is pure waste. Native zones keep theirs:
///     vmadm cannot create an OS zone imageless, and the recv
///     replaces the cloned root anyway.
///
/// The reservoir opt-in is intentionally not applied: the target
/// boots later via a plain `Start` job, which performs no
/// reservoir capacity check, so a reservoir-backed config could
/// fail to boot on a full host.
pub(crate) fn build_migration_target_payload(
    blueprint: &ProvisioningBlueprint,
    nic_tags: &NicTagMap,
) -> Result<serde_json::Value> {
    let mut payload = build_create_payload_with_nic_tags(blueprint, nic_tags, false)?;
    let obj = payload
        .as_object_mut()
        .ok_or_else(|| anyhow!("vmadm create payload is not a JSON object"))?;
    obj.insert("autoboot".to_string(), serde_json::Value::Bool(false));
    if let Some(disks) = obj.get_mut("disks").and_then(|d| d.as_array_mut()) {
        for disk in disks {
            if let Some(disk_obj) = disk.as_object_mut() {
                disk_obj.remove("image_uuid");
            }
        }
    }
    Ok(payload)
}

/// Idempotently set a string `attr` resource on a zone's zonecfg
/// (e.g. the bhyve brand's `migrate_listen` / `migrate_export`
/// boot flags, which vmadm payloads cannot express). `select`
/// only succeeds when the attr already exists, so first try an
/// in-place update and fall back to `add`. Names and values come
/// from agent code (never tenant input), so shell quoting is not
/// at risk; zonecfg takes the command string as a single argv
/// entry anyway.
pub async fn set_zone_attr(zone: Uuid, name: &str, value: &str) -> Result<()> {
    let id = zone.to_string();
    let update = format!("select attr name={name}; set value=\"{value}\"; end");
    let updated = Command::new("zonecfg")
        .args(["-z", &id, &update])
        .output()
        .await
        .with_context(|| format!("spawn zonecfg update attr {name} on {id}"))?;
    if updated.status.success() {
        return Ok(());
    }
    let add = format!("add attr; set name={name}; set type=string; set value=\"{value}\"; end");
    let added = Command::new("zonecfg")
        .args(["-z", &id, &add])
        .output()
        .await
        .with_context(|| format!("spawn zonecfg add attr {name} on {id}"))?;
    if added.status.success() {
        return Ok(());
    }
    let update_stderr = String::from_utf8_lossy(&updated.stderr).into_owned();
    let add_stderr = String::from_utf8_lossy(&added.stderr).into_owned();
    Err(anyhow!(
        "zonecfg set attr {name}={value} on zone {id} failed: update (exit {}): {}; add (exit {}): {}",
        updated.status,
        update_stderr.trim(),
        added.status,
        add_stderr.trim(),
    ))
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

/// Run `vmadm start <uuid>`. Used for `JobKind::Start` jobs.
///
/// Boots an already-provisioned zone that is stopped. The zone and
/// its Proteus ports persist across a power cycle, so this is a
/// power-on only — no zone or port re-create (contrast
/// `create_zone`, which `JobKind::Provision` uses for first-time
/// create).
pub async fn start_zone(instance_id: Uuid) -> Result<()> {
    run_simple(&["start", &instance_id.to_string()]).await
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

/// The slice of `vmadm get` we need for a disk resize. All sizes are in
/// MiB, matching vmadm's flexible-disk units.
#[derive(serde::Deserialize)]
struct VmInfo {
    #[serde(default)]
    flexible_disk_size: u64,
    #[serde(default)]
    disks: Vec<VmDisk>,
}

#[derive(serde::Deserialize)]
struct VmDisk {
    path: String,
    #[serde(default)]
    boot: bool,
    /// Disk size in MiB.
    #[serde(default)]
    size: u64,
}

/// Read the parts of `vmadm get <uuid>` we need to resize a disk.
async fn get_vm(instance_id: Uuid) -> Result<VmInfo> {
    let id = instance_id.to_string();
    let output = Command::new("vmadm")
        .arg("get")
        .arg(&id)
        .output()
        .await
        .with_context(|| format!("spawn vmadm get {id}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        bail!("vmadm get {id} failed (exit {}): {stderr}", output.status);
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("parse vmadm get {id} JSON"))
}

/// Pipe a JSON payload to `vmadm update <uuid>` (vmadm reads the payload
/// from stdin to EOF, same contract as `vmadm create`).
async fn run_update(instance_id: Uuid, payload: serde_json::Value) -> Result<()> {
    let payload_bytes = serde_json::to_vec(&payload).context("serialise vmadm update payload")?;
    let id = instance_id.to_string();
    let mut child = Command::new("vmadm")
        .arg("update")
        .arg(&id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn vmadm update — is it on PATH on this host?")?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(&payload_bytes)
            .await
            .context("write vmadm update payload to stdin")?;
        stdin
            .shutdown()
            .await
            .context("close stdin to vmadm update — vmadm reads to EOF before acting")?;
    } else {
        bail!("vmadm update child had no stdin");
    }
    let output = child
        .wait_with_output()
        .await
        .context("await vmadm update completion")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        return Err(anyhow!(
            "vmadm update {id} failed (exit {}): stderr={stderr}; stdout={stdout}",
            output.status,
        ));
    }
    Ok(())
}

/// Grow an instance's boot disk to `new_size_bytes`. Used for
/// `JobKind::ResizeDisk`.
///
/// Two ordered `vmadm update`s: first enlarge `flexible_disk_size` so
/// the pool can hold the bigger disk, then grow the boot zvol into it
/// (the platform runs `zfs set volsize` + adjusts the refreservation).
/// Grow-only and idempotent — a re-run whose target is already
/// satisfied is a no-op. The running guest sees the new capacity only
/// after a reboot (bhyve reads the block device size at boot); cloud-init
/// then grows the partition + filesystem.
pub async fn grow_boot_disk(instance_id: Uuid, new_size_bytes: u64) -> Result<()> {
    let new_disk_mib = new_size_bytes / 1024 / 1024;
    let vm = get_vm(instance_id).await?;
    let boot = vm
        .disks
        .iter()
        .find(|d| d.boot)
        .ok_or_else(|| anyhow!("vmadm get {instance_id}: no boot disk to resize"))?;
    if new_disk_mib <= boot.size {
        info!(
            %instance_id,
            current_mib = boot.size,
            target_mib = new_disk_mib,
            "resize-disk: boot disk already at/above target; nothing to do",
        );
        return Ok(());
    }
    let boot_path = boot.path.clone();
    let other_mib: u64 = vm.disks.iter().filter(|d| !d.boot).map(|d| d.size).sum();
    let new_flex = (new_disk_mib + other_mib).max(vm.flexible_disk_size);

    if new_flex > vm.flexible_disk_size {
        run_update(
            instance_id,
            serde_json::json!({ "flexible_disk_size": new_flex }),
        )
        .await
        .with_context(|| format!("grow flexible_disk_size to {new_flex} MiB"))?;
    }
    run_update(
        instance_id,
        serde_json::json!({
            "update_disks": [ { "path": boot_path, "size": new_disk_mib } ]
        }),
    )
    .await
    .with_context(|| format!("grow boot disk {boot_path} to {new_disk_mib} MiB"))?;

    info!(
        %instance_id,
        new_disk_mib,
        new_flex,
        "resize-disk: grew boot zvol + flexible pool",
    );
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
    build_create_payload_with_nic_tags(blueprint, &nic_tags, false)
}

pub(crate) fn build_create_payload_with_nic_tags(
    blueprint: &ProvisioningBlueprint,
    nic_tags: &NicTagMap,
    use_reservoir: bool,
) -> Result<serde_json::Value> {
    if image_brand(blueprint) == Some("bhyve") {
        create_bhyve_payload_with_nic_tags(blueprint, nic_tags, use_reservoir)
    } else {
        // Reservoir applies to bhyve guests only; native zones ignore it.
        create_zone_payload(blueprint)
    }
}

/// Whether this blueprint provisions a bhyve guest (the only brand that
/// draws from the memory reservoir). Used by the provision path to gate
/// reservoir growth + the `use_reservoir` payload flag.
pub(crate) fn blueprint_is_bhyve(blueprint: &ProvisioningBlueprint) -> bool {
    image_brand(blueprint) == Some("bhyve")
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
        // Pull customer_metadata back out so the IMDS fold can write
        // into the same map the SSH-keys block produced. None of the
        // earlier identity inserts use the metadata keys an operator
        // can set (the `tritond.*` prefix is the namespace barrier).
        let mut customer_metadata = obj
            .remove("customer_metadata")
            .and_then(|v| match v {
                serde_json::Value::Object(m) => Some(m),
                _ => None,
            })
            .unwrap_or_default();
        apply_provision_metadata(&mut customer_metadata, &mut internal_metadata, blueprint);
        obj.insert(
            "customer_metadata".to_string(),
            serde_json::Value::Object(customer_metadata),
        );
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
    create_bhyve_payload_with_nic_tags(blueprint, &nic_tags, false)
}

pub(crate) fn create_bhyve_payload_with_nic_tags(
    blueprint: &ProvisioningBlueprint,
    nic_tags: &NicTagMap,
    use_reservoir: bool,
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
    let root_pw = root_pw_from_metadata(blueprint);
    let permit_root_ssh = permit_root_ssh_from_metadata(blueprint);
    customer_metadata.insert(
        "cloud-init:user-data".to_string(),
        serde_json::Value::String(render_nocloud_user_data(
            &blueprint.ssh_public_keys,
            root_pw.as_deref(),
            permit_root_ssh,
        )),
    );
    customer_metadata.insert(
        "cloud-init:meta-data".to_string(),
        serde_json::Value::String(render_nocloud_meta_data(instance.id, &alias)),
    );
    // Supply our own network-config so the platform's NoCloud builder
    // uses it verbatim instead of auto-generating one with the
    // deprecated `gateway4:` key (which modern netplan rejects).
    customer_metadata.insert(
        "cloud-init:network-config".to_string(),
        serde_json::Value::String(render_nocloud_network_config(
            &blueprint.nics,
            &blueprint.subnets,
        )?),
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
    apply_provision_metadata(&mut customer_metadata, &mut internal_metadata, blueprint);

    let mut payload = serde_json::json!({
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
                // Boot zvol = the full flexible-disk budget so the guest
                // gets its package's disk, not the image's tiny content
                // size. The platform auto-adds a 16 MiB NoCloud seed disk
                // AND bumps flexible_disk_size by the same 16 to fit it
                // (cloudinit/nocloud.js updatePayloadDisks), so a boot
                // size == flexible_disk_size fills the pool exactly
                // (boot + seed == bumped flexible). vmadm requires a
                // numeric MiB size; there is no "remaining" keyword.
                "size": flexible_disk_size,
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
    });

    // Draw guest memory from the bhyve memory reservoir (RFD 0185). The
    // bhyve brand maps the zonecfg `bhyve_extra_opts` attr straight onto
    // the bhyve command line, so `-o memory.use_reservoir=true` reaches
    // bhyve with no platform change. Set only when the CN's effective
    // policy enables the reservoir AND the agent has confirmed/grown
    // enough free reservoir for this guest (the kernel does not fall back
    // to transient memory). Applied at create only.
    if use_reservoir && let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "bhyve_extra_opts".to_string(),
            serde_json::Value::String("-o memory.use_reservoir=true".to_string()),
        );
    }
    Ok(payload)
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
            "NIC {} has no IPv4 — vmadm payload requires v4 (v6-only zones not supported)",
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

fn render_nocloud_user_data(
    ssh_keys: &[String],
    root_pw: Option<&str>,
    permit_root_ssh: bool,
) -> String {
    let mut out = String::from("#cloud-config\ndisable_root: false\n");
    if !ssh_keys.is_empty() {
        out.push_str("ssh_authorized_keys:\n");
        for key in ssh_keys {
            out.push_str("  - ");
            out.push_str(key);
            out.push('\n');
        }
    }
    // `root_pw` lives in internal_metadata (a SmartOS zone-brand concept
    // the platform does NOT apply to a bhyve Linux guest), so for a
    // cloud-init guest we set the root password via `chpasswd`. `type:
    // text` takes the plaintext as-is (the value is already
    // operator-visible in instance metadata); `expire: false` skips the
    // force-change-on-first-login prompt. The password works at the
    // CONSOLE unconditionally (break-glass).
    //
    // Reaching root over SSH with that password is gated behind the
    // `instance/permit_root_ssh` opt-in: by default Ubuntu's
    // `prohibit-password` refuses root password SSH, and we leave it
    // there (use injected SSH keys for the normal path). When the
    // operator opts in we enable `PasswordAuthentication` (`ssh_pwauth`)
    // AND drop a `sshd_config.d` snippet flipping `PermitRootLogin` to
    // `yes`, then reload — both are required, `ssh_pwauth` alone does not
    // touch `PermitRootLogin`.
    if let Some(pw) = root_pw {
        out.push_str(
            "chpasswd:\n  expire: false\n  users:\n    - name: root\n      type: text\n      password: ",
        );
        push_yaml_double_quoted(&mut out, pw);
        out.push('\n');
        if permit_root_ssh {
            out.push_str(
                "ssh_pwauth: true\nwrite_files:\n  - path: /etc/ssh/sshd_config.d/60-tritonagent-root.conf\n    content: \"PermitRootLogin yes\\n\"\nruncmd:\n  - [ systemctl, restart, ssh ]\n",
            );
        }
    }
    // Ubuntu 26.04 (cloud-init 26.1) renders but refuses to APPLY the
    // network config on first boot: the NoCloud datasource sets
    // `previous_iid` in the early Local stage, so the Network stage's
    // default `boot-new-instance` gate wrongly concludes "not a new
    // instance", logs "No network config applied", and never brings the
    // interface up — leaving the static IP unbound (cloud-init #6666).
    // Widening the apply policy to every `boot` event defeats the gate.
    out.push_str("updates:\n  network:\n    when:\n      - boot\n");
    out
}

/// Append `s` as a YAML double-quoted scalar, escaping `\` and `"` so an
/// arbitrary password can't break out of the value (or the document).
fn push_yaml_double_quoted(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
}

/// The operator-set instance root password, stored as the
/// `instance/root_pw` provision-metadata entry (`guest_visible=false`).
/// `None` when unset.
fn root_pw_from_metadata(blueprint: &ProvisioningBlueprint) -> Option<String> {
    blueprint
        .provision_metadata
        .iter()
        .find(|e| e.key == "instance/root_pw")
        .map(|e| render_meta_value_as_string(&e.value))
}

/// Whether the operator opted this instance into root password SSH via
/// the `instance/permit_root_ssh` metadata key. Absent (the default)
/// means false: the root password is console-only break-glass, and SSH
/// stays at Ubuntu's `prohibit-password`. Accepts a JSON bool `true` or
/// the strings `true`/`1`/`yes`/`on` (case-insensitive); anything else,
/// including a missing key, is false (fail-closed to the safer posture).
fn permit_root_ssh_from_metadata(blueprint: &ProvisioningBlueprint) -> bool {
    blueprint
        .provision_metadata
        .iter()
        .find(|e| e.key == "instance/permit_root_ssh")
        .is_some_and(|e| match &e.value {
            serde_json::Value::Bool(b) => *b,
            other => matches!(
                render_meta_value_as_string(other)
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
                "true" | "1" | "yes" | "on"
            ),
        })
}

fn render_nocloud_meta_data(instance_id: Uuid, hostname: &str) -> String {
    format!("instance-id: {instance_id}\nlocal-hostname: {hostname}\n")
}

/// Build a cloud-init NoCloud `network-config` (netplan v2) for the
/// guest from its NICs, emitted as the `cloud-init:network-config`
/// customer_metadata key. The platform's NoCloud seed builder
/// (`nocloud.js`) passes a caller-supplied `cloud-init:network-config`
/// through verbatim, overriding its own auto-generated config.
///
/// Three deliberate choices, each working around a netplan/cloud-init
/// behaviour that otherwise leaves the guest with no IPv4 (verified live
/// on Ubuntu 26.04 / cloud-init 26.1):
///
///   * **Match by `driver`, never `macaddress`.** Since netplan 0.106 a
///     `macaddress` match renders as systemd-networkd
///     `PermanentMACAddress=`, resolved via ethtool `ETHTOOL_GPERMADDR`.
///     A bhyve virtio NIC has no permanent MAC (`00:00:00:00:00:00`), so
///     the `.network` never binds and the static address — though
///     rendered — is never applied (Launchpad #2022947, WONT FIX).
///   * **No `set-name`.** Renaming triggers the resolute netplan
///     "Unable to rename … [busy]" regression (cloud-init #6887). The
///     guest interface name is irrelevant to the host-side OPTE port,
///     which keys on MAC.
///   * **`routes:`, never `gateway4:`.** `gateway4` is deprecated and
///     rejected by modern netplan.
///
/// The companion first-boot apply gate (cloud-init #6666) is handled in
/// [`render_nocloud_user_data`].
fn render_nocloud_network_config(nics: &[Nic], subnets: &[Subnet]) -> Result<String> {
    let mut out = String::from("version: 2\nethernets:\n");
    for (index, nic) in nics.iter().enumerate() {
        let iface = format!("net{index}");
        let (ip_cidr, gateway) = bhyve_ipv4_config(nic, subnets)?;
        out.push_str(&format!("  {iface}:\n"));
        out.push_str("    match:\n      driver: virtio_net\n");
        out.push_str("    dhcp4: false\n    dhcp6: false\n    accept-ra: false\n");
        out.push_str("    addresses:\n");
        out.push_str(&format!("      - {ip_cidr}\n"));
        // The default route + resolvers live on the primary NIC only,
        // matching the single-`gateways` placement in `build_bhyve_nic_json`.
        if index == 0 {
            out.push_str("    routes:\n      - to: default\n");
            out.push_str(&format!("        via: {gateway}\n"));
            // TODO: thread resolvers from subnet/VPC config once vNext DNS
            // lands; a public default keeps the guest usable until then.
            out.push_str(
                "    nameservers:\n      addresses:\n        - 1.1.1.1\n        - 8.8.8.8\n",
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::net::Ipv4Addr;
    use tritond_client::types::{DiskKind, ImageCompatibility, Instance, JobKind, MetaEntry};

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
            kind: tritond_client::types::NetworkKind::Internal,
            nic_tag: None,
            vlan_id: None,
            provision_start_ipv4: None,
            provision_end_ipv4: None,
            provision_start_ipv6: None,
            provision_end_ipv6: None,
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
            imds_bindings: Vec::new(),
            provision_metadata: Vec::new(),
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
    fn bhyve_network_config_binds_static_ipv4_on_modern_netplan() {
        let bp = sample_bhyve_blueprint();
        let payload = create_bhyve_payload(&bp).unwrap();
        let netcfg = payload["customer_metadata"]["cloud-init:network-config"]
            .as_str()
            .expect("network-config present");
        // Match by driver, never macaddress: netplan >=0.106 renders a
        // macaddress match as `PermanentMACAddress=`, which a virtio NIC
        // (no ethtool permanent MAC) never satisfies, so the static
        // address renders but never binds (Launchpad #2022947).
        assert!(netcfg.contains("driver: virtio_net"), "{netcfg}");
        assert!(!netcfg.contains("macaddress"), "{netcfg}");
        // No set-name (cloud-init #6887 rename-busy regression).
        assert!(!netcfg.contains("set-name"), "{netcfg}");
        // Default route as a `routes:` entry, never deprecated gateway4/6.
        assert!(netcfg.contains("- to: default"), "{netcfg}");
        assert!(netcfg.contains("via: 10.199.199.1"), "{netcfg}");
        assert!(!netcfg.contains("gateway4"), "{netcfg}");
        assert!(!netcfg.contains("gateway6"), "{netcfg}");
        // The static address is what gives the guest its IP.
        assert!(netcfg.contains("- 10.199.199.77/24"), "{netcfg}");

        // The companion first-boot apply gate (#6666) lives in user-data.
        let userdata = payload["customer_metadata"]["cloud-init:user-data"]
            .as_str()
            .expect("user-data present");
        assert!(userdata.contains("updates:"), "{userdata}");
        assert!(userdata.contains("network:"), "{userdata}");
        assert!(userdata.contains("- boot"), "{userdata}");
    }

    #[test]
    fn user_data_root_pw_console_only_by_default() {
        // With a root_pw but no opt-in, the password is set (console
        // break-glass) but root SSH stays at Ubuntu's prohibit-password:
        // no ssh_pwauth, no PermitRootLogin snippet.
        let ud = render_nocloud_user_data(&[], Some("p@ss\"w0rd"), false);
        assert!(ud.contains("chpasswd:"), "{ud}");
        assert!(ud.contains("name: root"), "{ud}");
        // Double-quoted, with the embedded quote escaped so it can't
        // break the YAML.
        assert!(ud.contains("password: \"p@ss\\\"w0rd\""), "{ud}");
        assert!(!ud.contains("ssh_pwauth"), "{ud}");
        assert!(!ud.contains("PermitRootLogin"), "{ud}");
        assert!(!ud.contains("sshd_config.d"), "{ud}");
    }

    #[test]
    fn user_data_permit_root_ssh_opt_in_enables_password_login() {
        // With the opt-in, both PasswordAuthentication (ssh_pwauth) and
        // PermitRootLogin yes are emitted (ssh_pwauth alone leaves
        // PermitRootLogin at prohibit-password).
        let ud = render_nocloud_user_data(&[], Some("hunter2"), true);
        assert!(ud.contains("chpasswd:"), "{ud}");
        assert!(ud.contains("ssh_pwauth: true"), "{ud}");
        assert!(
            ud.contains("/etc/ssh/sshd_config.d/60-tritonagent-root.conf"),
            "{ud}"
        );
        assert!(ud.contains("PermitRootLogin yes"), "{ud}");
        assert!(ud.contains("systemctl, restart, ssh"), "{ud}");
        // The opt-in is inert without a password (nothing to log in with).
        let no_pw = render_nocloud_user_data(&[], None, true);
        assert!(!no_pw.contains("PermitRootLogin"), "{no_pw}");
        assert!(!no_pw.contains("ssh_pwauth"), "{no_pw}");
        // Without a root_pw at all, no password machinery is emitted.
        let none = render_nocloud_user_data(&[], None, false);
        assert!(!none.contains("chpasswd"), "{none}");
        assert!(!none.contains("ssh_pwauth"), "{none}");
        assert!(!none.contains("PermitRootLogin"), "{none}");
    }

    #[test]
    fn permit_root_ssh_metadata_parses_truthy_and_fails_closed() {
        fn bp_with(key: &str, value: serde_json::Value) -> ProvisioningBlueprint {
            let mut bp = sample_blueprint();
            bp.provision_metadata.push(MetaEntry {
                key: key.to_string(),
                value,
                guest_visible: true,
                guest_writable: false,
                updated_by: "test".to_string(),
                updated_at: Utc::now(),
            });
            bp
        }
        // Missing key -> false (the default, fail-closed).
        assert!(!permit_root_ssh_from_metadata(&sample_blueprint()));
        // JSON bool true, plus the accepted string spellings.
        for v in [
            serde_json::Value::Bool(true),
            serde_json::json!("true"),
            serde_json::json!("True"),
            serde_json::json!("YES"),
            serde_json::json!("1"),
            serde_json::json!(" on "),
        ] {
            assert!(
                permit_root_ssh_from_metadata(&bp_with("instance/permit_root_ssh", v.clone())),
                "{v:?} should be truthy"
            );
        }
        // Anything else fails closed.
        for v in [
            serde_json::Value::Bool(false),
            serde_json::json!("false"),
            serde_json::json!("nope"),
            serde_json::json!(0),
        ] {
            assert!(
                !permit_root_ssh_from_metadata(&bp_with("instance/permit_root_ssh", v.clone())),
                "{v:?} should be falsey"
            );
        }
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
                    "size": 20 * 1024,
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
                "cloud-init:user-data": concat!(
                    "#cloud-config\n",
                    "disable_root: false\n",
                    "ssh_authorized_keys:\n",
                    "  - ssh-ed25519 AAAA test@host\n",
                    "updates:\n",
                    "  network:\n",
                    "    when:\n",
                    "      - boot\n",
                ),
                "cloud-init:meta-data": format!(
                    "instance-id: {}\nlocal-hostname: smoke-zone\n",
                    instance.id,
                ),
                "cloud-init:network-config": concat!(
                    "version: 2\n",
                    "ethernets:\n",
                    "  net0:\n",
                    "    match:\n",
                    "      driver: virtio_net\n",
                    "    dhcp4: false\n",
                    "    dhcp6: false\n",
                    "    accept-ra: false\n",
                    "    addresses:\n",
                    "      - 10.199.199.77/24\n",
                    "    routes:\n",
                    "      - to: default\n",
                    "        via: 10.199.199.1\n",
                    "    nameservers:\n",
                    "      addresses:\n",
                    "        - 1.1.1.1\n",
                    "        - 8.8.8.8\n",
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
    fn migration_target_payload_bhyve_is_unbooted_and_imageless() {
        let bp = sample_bhyve_blueprint();
        let nic_tags = NicTagMap::new();
        let payload = build_migration_target_payload(&bp, &nic_tags).unwrap();
        assert_eq!(payload["brand"], "bhyve");
        assert_eq!(payload["autoboot"], false);
        // The disks are destroyed right after create (the recv
        // replaces them), so no image clone; and the disk budget
        // must survive image_uuid removal so the zvol skeleton has
        // the right size.
        let disk0 = &payload["disks"][0];
        assert!(disk0.get("image_uuid").is_none());
        assert_eq!(disk0["size"], 20 * 1024);
        assert_eq!(disk0["boot"], true);
        // Never reservoir-backed (a plain Start does no capacity check).
        assert!(payload.get("bhyve_extra_opts").is_none());
    }

    #[test]
    fn migration_target_payload_native_keeps_image_uuid() {
        let bp = sample_blueprint();
        let nic_tags = NicTagMap::new();
        let payload = build_migration_target_payload(&bp, &nic_tags).unwrap();
        assert_eq!(payload["brand"], "joyent-minimal");
        assert_eq!(payload["autoboot"], false);
        // OS zones cannot be created imageless.
        assert_eq!(
            payload["image_uuid"],
            bp.image.as_ref().unwrap().id.to_string()
        );
    }

    #[test]
    fn create_bhyve_payload_uses_proteus_nic_tags_when_supplied() {
        let bp = sample_bhyve_blueprint();
        let mut nic_tags = NicTagMap::new();
        nic_tags.insert(bp.nics[0].id, "proteus49377".to_string());

        let payload = create_bhyve_payload_with_nic_tags(&bp, &nic_tags, false).unwrap();

        assert_eq!(payload["nics"][0]["nic_tag"], "proteus49377");
    }

    #[test]
    fn create_bhyve_payload_sets_reservoir_opt_only_when_requested() {
        let bp = sample_bhyve_blueprint();
        let nic_tags = NicTagMap::new();

        let off = create_bhyve_payload_with_nic_tags(&bp, &nic_tags, false).unwrap();
        assert!(off.get("bhyve_extra_opts").is_none());

        let on = create_bhyve_payload_with_nic_tags(&bp, &nic_tags, true).unwrap();
        assert_eq!(on["bhyve_extra_opts"], "-o memory.use_reservoir=true");
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

        let err = create_bhyve_payload_with_nic_tags(&bp, &nic_tags, false).unwrap_err();

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
