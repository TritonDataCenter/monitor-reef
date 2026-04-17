// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_create` — the central provisioning task.
//!
//! Pipeline matches the legacy task:
//!
//! 1. **Pre-check** — zone with this UUID must not already exist (either
//!    via zoneadm or as a zfs dataset). AGENT-640 guards against
//!    concurrent creates by touching `/var/tmp/machine-creation-<uuid>`;
//!    the file is unlinked on success and cleaned up at startup.
//! 2. **Ensure image dataset** — if the payload references an image, make
//!    sure `<zpool>/<image_uuid>` exists; import it via imgadm if not.
//! 3. **Firewall rules** — if the payload carries `firewall_rules`, PUT
//!    each to the local firewaller on port 2021.
//! 4. **vmadm create** — send the scrubbed payload to `vmadm create` on
//!    stdin.
//! 5. **Reload** — `vmadm get <uuid>` and return `{vm: ...}`.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};

use crate::registry::TaskHandler;
use crate::smartos::firewaller::{FirewallRule, FirewallerClient};
use crate::smartos::imgadm_tool::{
    DEFAULT_ZPOOL, ImgadmTool, ImportOptions, default_install_dataset,
};
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{LoadOptions, VmadmTool};
use crate::smartos::zfs::{DatasetType, ListDatasetsOptions, ZfsError, ZfsTool};

/// Directory that holds the creation guard files (AGENT-640).
pub const DEFAULT_GUARD_DIR: &str = "/var/tmp";

/// Prefix of the per-UUID guard filename.
pub const GUARD_FILE_PREFIX: &str = "machine-creation-";

pub struct MachineCreateTask {
    vmadm: Arc<VmadmTool>,
    zfs: Arc<ZfsTool>,
    imgadm: Arc<ImgadmTool>,
    /// Admin IP the firewaller listens on. Resolved at startup and
    /// injected here; tests pass `127.0.0.1`.
    admin_ip: Ipv4Addr,
    /// Directory where the creation-guard files live. Defaults to
    /// `/var/tmp` — AGENT-640.
    guard_dir: PathBuf,
}

impl MachineCreateTask {
    pub fn new(
        vmadm: Arc<VmadmTool>,
        zfs: Arc<ZfsTool>,
        imgadm: Arc<ImgadmTool>,
        admin_ip: Ipv4Addr,
    ) -> Self {
        Self {
            vmadm,
            zfs,
            imgadm,
            admin_ip,
            guard_dir: PathBuf::from(DEFAULT_GUARD_DIR),
        }
    }

    pub fn with_guard_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.guard_dir = dir.into();
        self
    }
}

#[async_trait]
impl TaskHandler for MachineCreateTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let uuid = params
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TaskError::new("missing params.uuid".to_string()))?
            .to_string();
        let zpool = params
            .get("zfs_storage_pool_name")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_ZPOOL)
            .to_string();
        let include_dni = params
            .get("include_dni")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let guard = GuardFile::create(&self.guard_dir, &uuid).await?;

        let result = self.run_pipeline(&params, &uuid, &zpool, include_dni).await;

        guard.unlink().await;
        result
    }
}

/// RAII-ish helper for the provision guard file. Dropping without
/// unlink() is allowed — the startup path clears stale files via
/// [`cleanup_stale_guards`].
#[derive(Debug)]
struct GuardFile {
    path: PathBuf,
}

impl GuardFile {
    async fn create(dir: &Path, uuid: &str) -> Result<Self, TaskError> {
        let path = dir.join(format!("{GUARD_FILE_PREFIX}{uuid}"));
        // We use O_EXCL semantics: fail if someone else already has this
        // guard, so concurrent creates don't silently race.
        match tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
        {
            Ok(_) => Ok(Self { path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(TaskError::new(format!(
                    "another machine_create is in progress for {uuid} ({})",
                    path.display()
                )))
            }
            Err(e) => Err(TaskError::new(format!(
                "failed to create guard file {}: {e}",
                path.display()
            ))),
        }
    }

    async fn unlink(self) {
        if let Err(e) = tokio::fs::remove_file(&self.path).await {
            tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "failed to remove machine-creation guard"
            );
        }
    }
}

impl MachineCreateTask {
    async fn run_pipeline(
        &self,
        params: &serde_json::Value,
        uuid: &str,
        zpool: &str,
        include_dni: bool,
    ) -> Result<TaskResult, TaskError> {
        self.pre_check(uuid, zpool).await?;
        self.ensure_image_present(params, zpool).await?;
        self.apply_firewall_rules(params).await?;

        let payload = scrub_payload(params.clone());
        self.vmadm
            .create(&payload)
            .await
            .map_err(|e| vmadm_error_to_task(e, "vmadm.create error"))?;

        let load_opts = LoadOptions {
            include_dni,
            fields: None,
        };
        let vm = self
            .vmadm
            .load(uuid, &load_opts)
            .await
            .map_err(|e| vmadm_error_to_task(e, "vmadm.load error"))?;

        Ok(serde_json::json!({ "vm": vm }))
    }

    /// Reject preconditions before we touch the system: the VM must
    /// not already exist (even DNI), and the zone dataset must not
    /// already exist.
    async fn pre_check(&self, uuid: &str, zpool: &str) -> Result<(), TaskError> {
        // `vmadm.load` with include_dni=true picks up both regular and
        // DNI zones, matching the legacy check which used `zoneList`.
        let load_opts = LoadOptions {
            include_dni: true,
            fields: None,
        };
        match self.vmadm.load(uuid, &load_opts).await {
            Ok(_) => {
                return Err(TaskError::new(format!("Machine {uuid} exists.")));
            }
            Err(crate::smartos::vmadm::VmadmError::NotFound { .. }) => {}
            Err(e) => return Err(vmadm_error_to_task(e, "pre-check error")),
        }

        let dataset = default_install_dataset(zpool, uuid);
        let opts = ListDatasetsOptions {
            dataset: Some(dataset.clone()),
            kind: DatasetType::All,
            recursive: false,
        };
        match self.zfs.list_datasets(&opts).await {
            Ok(rows) if rows.is_empty() => Ok(()),
            Ok(_) => Err(TaskError::new(format!("Dataset {dataset} exists."))),
            Err(ZfsError::NonZeroExit { stderr, .. }) if stderr.contains("does not exist") => {
                Ok(())
            }
            Err(e) => Err(TaskError::new(format!("pre-check error: {e}"))),
        }
    }

    /// If the payload references an image (via `image_uuid` at the top
    /// level or via `disks[0].image_uuid` for KVM-style payloads), make
    /// sure it's present on-disk. Import it if not.
    async fn ensure_image_present(
        &self,
        params: &serde_json::Value,
        zpool: &str,
    ) -> Result<(), TaskError> {
        let image_uuid = params
            .get("image_uuid")
            .and_then(|v| v.as_str())
            .or_else(|| {
                params
                    .get("disks")
                    .and_then(|d| d.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|first| first.get("image_uuid"))
                    .and_then(|v| v.as_str())
            });
        let Some(image_uuid) = image_uuid else {
            // No image — raw dataset / VM, nothing to fetch.
            return Ok(());
        };

        let dataset = default_install_dataset(zpool, image_uuid);
        let exists = match self
            .zfs
            .list_datasets(&ListDatasetsOptions {
                dataset: Some(dataset.clone()),
                kind: DatasetType::All,
                recursive: false,
            })
            .await
        {
            Ok(rows) => !rows.is_empty(),
            Err(ZfsError::NonZeroExit { stderr, .. }) if stderr.contains("does not exist") => false,
            Err(e) => {
                return Err(TaskError::new(format!(
                    "failed to check for image dataset {dataset}: {e}"
                )));
            }
        };
        if exists {
            return Ok(());
        }

        self.imgadm
            .import(zpool, image_uuid, &ImportOptions::default())
            .await
            .map_err(|e| TaskError::new(format!("imgadm import failed: {e}")))
    }

    /// PUT each firewall rule to the local firewaller. Skips silently
    /// when the payload has no rules.
    async fn apply_firewall_rules(&self, params: &serde_json::Value) -> Result<(), TaskError> {
        let rules = params
            .get("firewall_rules")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if rules.is_empty() {
            return Ok(());
        }

        let request_id = params.get("req_id").and_then(|v| v.as_str());
        let client = FirewallerClient::new(self.admin_ip, request_id)
            .map_err(|e| TaskError::new(format!("build firewaller client: {e}")))?;

        let typed: Vec<FirewallRule> = rules
            .into_iter()
            .filter_map(|rule| {
                let uuid = rule.get("uuid").and_then(|v| v.as_str())?.to_string();
                Some(FirewallRule {
                    uuid,
                    payload: rule,
                })
            })
            .collect();

        client
            .put_rules(&typed)
            .await
            .map_err(|e| TaskError::new(format!("firewaller error: {e}")))
    }
}

/// Drop agent-layer fields vmadm doesn't accept in its payload.
///
/// The legacy code deleted `log`, `req_id`, and `vmadmLogger` before
/// passing to vmadm. We don't have `log` or `vmadmLogger`, but
/// `firewall_rules`, `include_dni`, and `zfs_storage_pool_name` are
/// agent concerns that don't belong in the vmadm payload either.
fn scrub_payload(mut params: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = params.as_object_mut() {
        for key in [
            "firewall_rules",
            "include_dni",
            "zfs_storage_pool_name",
            "imgapiPeers",
            "req_id",
        ] {
            obj.remove(key);
        }
    }
    params
}

/// Remove any stale `machine-creation-*` guards left behind by an
/// agent that exited mid-provision. Called from the startup pipeline
/// exactly once, matching `SmartosBackend.cleanupStaleLocks`.
pub async fn cleanup_stale_guards(dir: &Path) -> std::io::Result<()> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(s) = name.to_str() else { continue };
        if s.starts_with(GUARD_FILE_PREFIX) {
            let p = entry.path();
            if let Err(e) = tokio::fs::remove_file(&p).await {
                tracing::warn!(
                    path = %p.display(),
                    error = %e,
                    "failed to clean stale creation guard"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_payload_drops_agent_fields() {
        let cleaned = scrub_payload(serde_json::json!({
            "uuid": "abc",
            "brand": "joyent",
            "firewall_rules": [{"uuid": "x"}],
            "include_dni": false,
            "zfs_storage_pool_name": "zones",
            "imgapiPeers": [],
            "req_id": "r"
        }));
        let obj = cleaned.as_object().expect("object");
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("uuid"));
        assert!(obj.contains_key("brand"));
    }

    #[tokio::test]
    async fn cleanup_stale_guards_removes_matching_files() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        tokio::fs::write(tmp.path().join(format!("{GUARD_FILE_PREFIX}aaa")), b"")
            .await
            .expect("write guard");
        tokio::fs::write(tmp.path().join("unrelated.txt"), b"")
            .await
            .expect("write unrelated");

        cleanup_stale_guards(tmp.path()).await.expect("cleanup");

        assert!(!tmp.path().join(format!("{GUARD_FILE_PREFIX}aaa")).exists());
        assert!(tmp.path().join("unrelated.txt").exists());
    }

    #[tokio::test]
    async fn guard_file_exclusive_creation() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let uuid = "abc00000-1111-2222-3333-444444444444";
        let g1 = GuardFile::create(tmp.path(), uuid).await.expect("first");
        let err = GuardFile::create(tmp.path(), uuid).await.unwrap_err();
        assert!(err.error.contains("in progress"));
        g1.unlink().await;
        // After unlink, a new guard succeeds.
        let g2 = GuardFile::create(tmp.path(), uuid).await.expect("second");
        g2.unlink().await;
    }
}
