// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Disk-usage accounting for the heartbeater.
//!
//! Reproduces the legacy `getDiskUsage` computation (see
//! `sdc-cn-agent/lib/backends/smartos/index.js`) byte-for-byte:
//!
//! 1. Sum the `used`/`volsize` of every KVM VM's zvols.
//! 2. Sum the `quota`/`used` of every VM's zoneroot (KVM and native zones
//!    tracked in separate fields so DAPI can tell them apart).
//! 3. Sum the `cores` quotas.
//! 4. Sum the `used` bytes of every installed image (filtered via the
//!    imgadm on-disk manifest to avoid double-counting stub entries).
//! 5. Record pool size + pool allocation.
//! 6. Derive "system used" = pool_alloc − everything else, so anything the
//!    sampler doesn't know about (kernel dumps, platform logs, etc.) is
//!    attributed to the CN itself rather than silently leaking into VM
//!    totals.
//!
//! Keeping the field names identical means CNAPI/DAPI stay oblivious to
//! the rewrite.

use std::sync::Arc;

use thiserror::Error;

use crate::smartos::imgadm::ImgadmDb;
use crate::smartos::zfs::{ZfsError, ZfsTool};

use cn_agent_api::Uuid;

/// Disk-accounting fields posted to CNAPI.
///
/// Field names and ordering match the legacy `usage` object in
/// `lib/backends/smartos/index.js:getDiskUsage`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DiskUsage {
    pub kvm_zvol_used_bytes: u64,
    pub kvm_zvol_volsize_bytes: u64,
    pub kvm_quota_bytes: u64,
    pub kvm_quota_used_bytes: u64,
    pub zone_quota_bytes: u64,
    pub zone_quota_used_bytes: u64,
    pub cores_quota_bytes: u64,
    pub cores_quota_used_bytes: u64,
    pub installed_images_used_bytes: u64,
    pub pool_size_bytes: u64,
    pub pool_alloc_bytes: u64,
    pub system_used_bytes: u64,
}

/// Minimal view of a VM the sampler needs. Avoids coupling the accounting
/// logic to the full vmadm JSON payload.
#[derive(Debug, Clone)]
pub struct VmSnapshot {
    pub uuid: Uuid,
    pub brand: Option<String>,
    pub zonepath: Option<String>,
    /// Paths to every zvol attached to this VM (e.g., `/dev/zvol/rdsk/…`).
    /// Empty for non-KVM brands.
    pub disks: Vec<String>,
}

#[derive(Debug, Error)]
pub enum DiskUsageError {
    #[error("zfs tooling failed: {0}")]
    Zfs(#[from] ZfsError),
    #[error("zfs output missing property {0}")]
    MissingProperty(&'static str),
}

/// Properties pulled from `zfs get` for every dataset on the node.
///
/// Matches the legacy call site:
/// `zfs.get(null, ['name','used','avail','refer','type','mountpoint','quota','origin','volsize'], true, ...)`.
pub const DISK_PROPERTIES: &[&str] = &[
    "name",
    "used",
    "avail",
    "refer",
    "type",
    "mountpoint",
    "quota",
    "origin",
    "volsize",
];

/// Dataset name of the main zpool. On any Triton CN this is `zones` — the
/// legacy code hardcoded it too.
pub const ROOT_POOL_DATASET: &str = "zones";

/// Regex-style match for dataset names that look like imgadm-owned images.
///
/// Pattern: `<pool>/<uuid>` with a valid v4-ish UUID. We don't use the
/// `regex` crate yet; the `is_image_dataset_name` helper does the same job
/// in plain Rust.
fn is_image_dataset_name(name: &str) -> Option<Uuid> {
    let (_pool, rest) = name.split_once('/')?;
    if rest.contains('/') || rest.contains('@') {
        return None;
    }
    Uuid::parse_str(rest).ok()
}

/// zvol path prefix vmadm exposes for KVM disks.
const ZVOL_RDSK_PREFIX: &str = "/dev/zvol/rdsk/";

/// Sampler: holds the tools and computes [`DiskUsage`] on demand.
#[derive(Clone)]
pub struct DiskUsageSampler {
    zfs: Arc<ZfsTool>,
    imgadm: Arc<ImgadmDb>,
    /// Zpool dataset to treat as the root for the pool_size/pool_alloc
    /// fields. Defaults to `"zones"`.
    pool_dataset: String,
}

impl DiskUsageSampler {
    pub fn new(zfs: Arc<ZfsTool>, imgadm: Arc<ImgadmDb>) -> Self {
        Self {
            zfs,
            imgadm,
            pool_dataset: ROOT_POOL_DATASET.to_string(),
        }
    }

    pub fn with_pool_dataset(mut self, name: impl Into<String>) -> Self {
        self.pool_dataset = name.into();
        self
    }

    /// Compute disk usage for the given set of VMs.
    pub async fn sample(&self, vms: &[VmSnapshot]) -> Result<DiskUsage, DiskUsageError> {
        let datasets = self.zfs.get_all_properties(DISK_PROPERTIES).await?;

        let mut usage = DiskUsage::default();
        self.account_vms(&mut usage, vms, &datasets);
        self.account_installed_images(&mut usage, &datasets).await;
        self.account_pool(&mut usage, &datasets)?;

        // system_used = pool_alloc − (everything else we accounted for)
        // Saturate rather than wrap: if rounding leaves us slightly
        // negative (e.g., snapshots we didn't attribute), report 0 instead
        // of u64::MAX.
        let tracked = usage
            .kvm_zvol_used_bytes
            .saturating_add(usage.kvm_quota_used_bytes)
            .saturating_add(usage.zone_quota_used_bytes)
            .saturating_add(usage.cores_quota_used_bytes)
            .saturating_add(usage.installed_images_used_bytes);
        usage.system_used_bytes = usage.pool_alloc_bytes.saturating_sub(tracked);

        Ok(usage)
    }

    /// Tally VM-attributable bytes (disks, quotas, cores).
    fn account_vms(
        &self,
        usage: &mut DiskUsage,
        vms: &[VmSnapshot],
        datasets: &serde_json::Map<String, serde_json::Value>,
    ) {
        for vm in vms {
            let is_kvm = vm.brand.as_deref() == Some("kvm");

            if is_kvm {
                for disk in &vm.disks {
                    let Some(ds_name) = disk.strip_prefix(ZVOL_RDSK_PREFIX) else {
                        continue;
                    };
                    if let Some(ds) = datasets.get(ds_name) {
                        usage.kvm_zvol_used_bytes = usage
                            .kvm_zvol_used_bytes
                            .saturating_add(get_u64(ds, "used"));
                        usage.kvm_zvol_volsize_bytes = usage
                            .kvm_zvol_volsize_bytes
                            .saturating_add(get_u64(ds, "volsize"));
                    }
                }
            }

            if let Some(zoneroot_key) = vm.zonepath.as_deref().and_then(strip_leading_slash)
                && let Some(ds) = datasets.get(zoneroot_key)
            {
                let quota = get_u64(ds, "quota");
                let used = get_u64(ds, "used");
                if is_kvm {
                    usage.kvm_quota_bytes = usage.kvm_quota_bytes.saturating_add(quota);
                    usage.kvm_quota_used_bytes = usage.kvm_quota_used_bytes.saturating_add(used);
                } else {
                    usage.zone_quota_bytes = usage.zone_quota_bytes.saturating_add(quota);
                    usage.zone_quota_used_bytes = usage.zone_quota_used_bytes.saturating_add(used);
                }
            }

            // Cores datasets live in two historical locations depending on
            // platform age: inside the zone (`<zonepath>/cores`) or in a
            // flat per-VM dir under the pool (`zones/cores/<uuid>`).
            let cores_keys = [
                vm.zonepath
                    .as_deref()
                    .and_then(strip_leading_slash)
                    .map(|z| format!("{z}/cores")),
                Some(format!("{}/cores/{}", self.pool_dataset, vm.uuid)),
            ];
            for key in cores_keys.into_iter().flatten() {
                if let Some(ds) = datasets.get(key.as_str()) {
                    usage.cores_quota_bytes =
                        usage.cores_quota_bytes.saturating_add(get_u64(ds, "quota"));
                    usage.cores_quota_used_bytes = usage
                        .cores_quota_used_bytes
                        .saturating_add(get_u64(ds, "used"));
                    break;
                }
            }
        }
    }

    /// Walk every `<pool>/<uuid>` dataset; if imgadm has a real manifest
    /// for it, credit its bytes to `installed_images_used_bytes`.
    async fn account_installed_images(
        &self,
        usage: &mut DiskUsage,
        datasets: &serde_json::Map<String, serde_json::Value>,
    ) {
        for (name, ds) in datasets {
            let Some(uuid) = is_image_dataset_name(name) else {
                continue;
            };
            match self.imgadm.get(&uuid).await {
                Ok(entry) if entry.has_real_manifest() => {
                    usage.installed_images_used_bytes = usage
                        .installed_images_used_bytes
                        .saturating_add(get_u64(ds, "used"));
                }
                Ok(_) => {
                    // Stub manifest — not a real image, don't double-count.
                }
                Err(e) if e.is_not_installed() => {
                    // Dataset UUID isn't actually an image; ignore.
                }
                Err(e) => {
                    tracing::warn!(
                        dataset = %name,
                        error = %e,
                        "failed to read imgadm manifest while accounting disk usage"
                    );
                }
            }
        }
    }

    /// Pool-wide totals.
    fn account_pool(
        &self,
        usage: &mut DiskUsage,
        datasets: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), DiskUsageError> {
        let pool = datasets
            .get(self.pool_dataset.as_str())
            .ok_or(DiskUsageError::MissingProperty("zones"))?;
        let used = get_u64(pool, "used");
        let available = get_u64(pool, "avail");
        usage.pool_alloc_bytes = used;
        usage.pool_size_bytes = used.saturating_add(available);
        Ok(())
    }
}

/// Extract a numeric ZFS property from the nested `{prop: value}` map.
///
/// Missing or non-numeric properties are silently treated as 0, matching
/// the legacy `toInt()` helper that wrapped `parseInt(val, 10)`.
fn get_u64(ds: &serde_json::Value, prop: &str) -> u64 {
    let Some(s) = ds.get(prop).and_then(|v| v.as_str()) else {
        return 0;
    };
    s.parse::<u64>().unwrap_or(0)
}

fn strip_leading_slash(s: &str) -> Option<&str> {
    s.strip_prefix('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkprop(used: &str, avail: &str) -> serde_json::Value {
        serde_json::json!({"used": used, "avail": avail})
    }

    #[test]
    fn parses_uuid_style_image_dataset_names() {
        assert!(is_image_dataset_name("zones/5135b4bb-da9e-48e2-8965-7424267ad23e").is_some());
        assert!(is_image_dataset_name("zones/5135b4bb-da9e-48e2-8965-7424267ad23e/root").is_none());
        assert!(is_image_dataset_name("zones/cores").is_none());
        assert!(
            is_image_dataset_name("zones/5135b4bb-da9e-48e2-8965-7424267ad23e@snap1").is_none()
        );
    }

    #[test]
    fn get_u64_tolerates_missing_fields() {
        let ds = mkprop("1024", "512");
        assert_eq!(get_u64(&ds, "used"), 1024);
        assert_eq!(get_u64(&ds, "quota"), 0); // not present
    }

    #[test]
    fn strip_leading_slash_matches_legacy_slice1() {
        assert_eq!(strip_leading_slash("/zones/abc"), Some("zones/abc"));
        assert_eq!(strip_leading_slash("zones/abc"), None);
    }

    #[tokio::test]
    async fn sample_computes_zones_and_pool_totals() {
        // Build a sampler that can't actually read imgadm or zfs — we drive
        // it via a DiskUsage computed by hand. This is a narrow unit test
        // for the non-async accounting halves.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let imgadm = Arc::new(ImgadmDb::with_dir(tmp.path()));
        // Not used by the sub-function under test.
        let zfs = Arc::new(ZfsTool::with_bins("/nonexistent/zfs", "/nonexistent/zpool"));
        let sampler = DiskUsageSampler::new(zfs, imgadm);

        let mut datasets = serde_json::Map::new();
        datasets.insert(
            "zones".to_string(),
            serde_json::json!({"used": "100", "avail": "400"}),
        );
        datasets.insert(
            "zones/aaaaaaaa-1111-2222-3333-444444444444".to_string(),
            serde_json::json!({"used": "20", "type": "filesystem", "quota": "50"}),
        );

        let vms = vec![VmSnapshot {
            uuid: Uuid::parse_str("aaaaaaaa-1111-2222-3333-444444444444").expect("uuid"),
            brand: Some("joyent".into()),
            zonepath: Some("/zones/aaaaaaaa-1111-2222-3333-444444444444".into()),
            disks: vec![],
        }];

        let mut usage = DiskUsage::default();
        sampler.account_vms(&mut usage, &vms, &datasets);
        sampler.account_pool(&mut usage, &datasets).expect("pool");
        assert_eq!(usage.zone_quota_bytes, 50);
        assert_eq!(usage.zone_quota_used_bytes, 20);
        assert_eq!(usage.pool_alloc_bytes, 100);
        assert_eq!(usage.pool_size_bytes, 500);
    }

    #[tokio::test]
    async fn sample_attributes_kvm_zvols() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let imgadm = Arc::new(ImgadmDb::with_dir(tmp.path()));
        let zfs = Arc::new(ZfsTool::with_bins("/nonexistent/zfs", "/nonexistent/zpool"));
        let sampler = DiskUsageSampler::new(zfs, imgadm);

        let mut datasets = serde_json::Map::new();
        datasets.insert(
            "zones/bbbbbbbb-1111-2222-3333-444444444444-disk0".to_string(),
            serde_json::json!({"used": "500", "volsize": "1000", "type": "volume"}),
        );

        let vms = vec![VmSnapshot {
            uuid: Uuid::parse_str("bbbbbbbb-1111-2222-3333-444444444444").expect("uuid"),
            brand: Some("kvm".into()),
            zonepath: Some("/zones/bbbbbbbb-1111-2222-3333-444444444444".into()),
            disks: vec!["/dev/zvol/rdsk/zones/bbbbbbbb-1111-2222-3333-444444444444-disk0".into()],
        }];

        let mut usage = DiskUsage::default();
        sampler.account_vms(&mut usage, &vms, &datasets);
        assert_eq!(usage.kvm_zvol_used_bytes, 500);
        assert_eq!(usage.kvm_zvol_volsize_bytes, 1000);
    }
}
