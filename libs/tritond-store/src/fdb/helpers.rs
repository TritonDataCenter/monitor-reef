// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Private inherent-impl helpers shared across the `Store` trait
//! methods: opinionated wrappers around the FDB binding (e.g.
//! `read_bytes`, `scan_dhcp_leases`), and the per-scope `_inner`
//! helpers that consolidate image/ssh-key creates.

use super::*;
use crate::fdb_txn;

impl FdbStore {
    /// Read the value for a single key, returning `None` if absent.
    pub(super) async fn read_bytes(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let key = key.to_vec();
        let result: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;
        result.map_err(StoreError::from)
    }

    pub(super) async fn read_nat_gateway_record(
        &self,
        nat_gateway_id: Uuid,
    ) -> Result<NatGatewayRecord, StoreError> {
        let key = keys::nat_gateway_by_id_key(nat_gateway_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("nat gateway"))
    }

    /// Range-scan a `dhcp_lease/by_vpc/...` prefix and decode every
    /// value as a [`DhcpLease`]. Used by both the per-VPC list and
    /// the `list_all_dhcp_leases` reconciler-feeding scan.
    pub(super) async fn scan_dhcp_leases(&self, prefix: Vec<u8>) -> Result<Vec<DhcpLease>, StoreError> {
        let (begin, end) = prefix_range(&prefix);
        let values: Result<Vec<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok(kvs.iter().map(|kv| kv.value().to_vec()).collect())
                }
            })
            .await;
        let values = values.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(values.len());
        for bytes in values {
            let lease: DhcpLease = serde_json::from_slice(&bytes)
                .map_err(de_err("dhcp lease"))?;
            out.push(lease);
        }
        Ok(out)
    }

    /// Range-scan a `prefix` and return the raw value bytes of every
    /// key. Used by the fleet-wide registries (nic_tag, cn-nic-tags,
    /// network-pool) whose value rows are the full JSON record, so the
    /// caller deserialises directly without a second by_id read.
    pub(super) async fn scan_values(&self, prefix: Vec<u8>) -> Result<Vec<Vec<u8>>, StoreError> {
        let (begin, end) = prefix_range(&prefix);
        let values: Result<Vec<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok(kvs.iter().map(|kv| kv.value().to_vec()).collect())
                }
            })
            .await;
        values.map_err(StoreError::from)
    }

    pub(super) async fn read_edge_cluster_record(
        &self,
        edge_cluster_id: Uuid,
    ) -> Result<EdgeClusterRecord, StoreError> {
        let key = keys::edge_cluster_by_id_key(edge_cluster_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("edge cluster"))
    }

    /// Shared body for the per-scope `create_image_*` methods.
    /// Performs (in one transaction): optional parent-existence
    /// check, `(scope, name)` uniqueness check, id-uniqueness
    /// check, then writes `image/by_id`, the per-scope `by_*`
    /// name index, and the per-scope membership index.
    ///
    /// `in_scope_key_for` builds the membership-index key for a
    /// given image id; it's a closure so each per-scope caller
    /// can capture its own scope identity (silo / tenant / project
    /// / user uuid).
    pub(super) async fn create_image_inner<F>(
        &self,
        scope: ImageScope,
        req: NewImage,
        parent_check_key: Option<Vec<u8>>,
        by_name_key: Vec<u8>,
        in_scope_key_for: F,
        scope_label: &'static str,
    ) -> Result<Image, StoreError>
    where
        F: Fn(Uuid) -> Vec<u8> + Send + Sync,
    {
        let id = req
            .id
            .unwrap_or_else(|| crate::derive_image_id(&scope, &req.sha256));
        let image = Image {
            id,
            scope: scope.clone(),
            name: req.name.clone(),
            description: req.description.clone().unwrap_or_default(),
            os: req.os.clone(),
            version: req.version.clone(),
            size_bytes: req.size_bytes,
            sha256: req.sha256.clone(),
            source_url: req.source_url.clone(),
            compatibility: req.compatibility.clone(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&image)
            .map_err(ser_err("image"))?;
        let by_id_key = keys::image_by_id_key(image.id);
        let in_scope_key = in_scope_key_for(image.id);
        let id_str = image.id.to_string();

        enum Outcome {
            Created,
            ParentMissing,
            NameTaken,
            IdTaken,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_scope_key = in_scope_key.clone();
                let parent_check_key = parent_check_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if let Some(pkey) = parent_check_key.as_ref()
                        && tr.get(pkey, false).await?.is_none()
                    {
                        return Ok(Outcome::ParentMissing);
                    }
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }
                    if tr.get(&by_id_key, false).await?.is_some() {
                        return Ok(Outcome::IdTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_scope_key, b"");
                    Ok(Outcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created) => Ok(image),
            Ok(Outcome::ParentMissing) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "image with name {:?} already exists in {scope_label} scope",
                req.name,
            ))),
            Ok(Outcome::IdTaken) => Err(StoreError::Conflict(format!(
                "image with id {} already exists",
                image.id,
            ))),
            Err(e) => Err(e.into()),
        }
    }

    /// Shared body for the per-scope `list_images_*` methods.
    /// Walks a `image/in_*` membership-index prefix, parses the
    /// suffix uuids, then fetches each image record by id.
    pub(super) async fn list_images_via_index(&self, prefix: Vec<u8>) -> Result<Vec<Image>, StoreError> {
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();
        let id_strs: Result<Vec<String>, FdbBindingError> = fdb_txn!(self.db, [begin, end], |tr| {
            let opt = RangeOption {
                begin: KeySelector::first_greater_or_equal(begin),
                end: KeySelector::first_greater_or_equal(end),
                ..RangeOption::default()
            };
            let kvs = tr.get_range(&opt, 1, false).await?;
            let mut ids = Vec::new();
            for kv in kvs.iter() {
                let suffix = &kv.key()[prefix_len..];
                if let Ok(s) = std::str::from_utf8(suffix) {
                    ids.push(s.to_string());
                }
            }
            Ok(ids)
        })
            ;
        let id_strs = id_strs.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("image index uuid: {e}")))?;
            let by_id_key = keys::image_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let image: Image = serde_json::from_slice(&bytes)
                    .map_err(de_err("image"))?;
                out.push(image);
            }
        }
        Ok(out)
    }

    /// Shared body for the per-scope `create_ssh_key_*` methods.
    /// Mirrors [`Self::create_image_inner`]: optional parent
    /// existence check, then writes `ssh_key/by_id`, the
    /// per-scope name index, the per-scope fingerprint index,
    /// and the per-scope membership index. The id is
    /// content-addressed via [`crate::derive_ssh_key_id`] so
    /// idempotent re-create yields the same record.
    #[allow(clippy::too_many_arguments)] // 8 args is the natural shape for this helper.
    pub(super) async fn create_ssh_key_inner<F>(
        &self,
        scope: SshKeyScope,
        req: NewSshKey,
        fingerprint: String,
        parent_check_key: Option<Vec<u8>>,
        by_name_key: Vec<u8>,
        by_fp_key: Vec<u8>,
        in_scope_key_for: F,
        scope_label: &'static str,
    ) -> Result<SshKey, StoreError>
    where
        F: Fn(Uuid) -> Vec<u8> + Send + Sync,
    {
        let id = crate::derive_ssh_key_id(&scope, &fingerprint);
        let key = SshKey {
            id,
            scope: scope.clone(),
            name: req.name.clone(),
            description: req.description.clone().unwrap_or_default(),
            public_key: req.public_key.clone(),
            fingerprint: fingerprint.clone(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&key)
            .map_err(ser_err("ssh key"))?;
        let by_id_key = keys::ssh_key_by_id_key(key.id);
        let in_scope_key = in_scope_key_for(key.id);
        let id_str = key.id.to_string();

        enum Outcome {
            Created,
            ParentMissing,
            NameTaken,
            FingerprintTaken,
            IdTaken,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let by_fp_key = by_fp_key.clone();
                let in_scope_key = in_scope_key.clone();
                let parent_check_key = parent_check_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if let Some(pkey) = parent_check_key.as_ref()
                        && tr.get(pkey, false).await?.is_none()
                    {
                        return Ok(Outcome::ParentMissing);
                    }
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }
                    if tr.get(&by_fp_key, false).await?.is_some() {
                        return Ok(Outcome::FingerprintTaken);
                    }
                    if tr.get(&by_id_key, false).await?.is_some() {
                        return Ok(Outcome::IdTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&by_fp_key, &id_bytes);
                    tr.set(&in_scope_key, b"");
                    Ok(Outcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created) => Ok(key),
            Ok(Outcome::ParentMissing) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "ssh key with name {:?} already exists in {scope_label} scope",
                req.name,
            ))),
            Ok(Outcome::FingerprintTaken) => Err(StoreError::Conflict(format!(
                "ssh key with fingerprint {fingerprint} already exists in {scope_label} scope",
            ))),
            Ok(Outcome::IdTaken) => Err(StoreError::Conflict(format!(
                "ssh key with id {} already exists",
                key.id,
            ))),
            Err(e) => Err(e.into()),
        }
    }

    /// Shared body for the per-scope `list_ssh_keys_*` methods.
    /// Mirrors [`Self::list_images_via_index`].
    pub(super) async fn list_ssh_keys_via_index(&self, prefix: Vec<u8>) -> Result<Vec<SshKey>, StoreError> {
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();
        let id_strs: Result<Vec<String>, FdbBindingError> = fdb_txn!(self.db, [begin, end], |tr| {
            let opt = RangeOption {
                begin: KeySelector::first_greater_or_equal(begin),
                end: KeySelector::first_greater_or_equal(end),
                ..RangeOption::default()
            };
            let kvs = tr.get_range(&opt, 1, false).await?;
            let mut ids = Vec::new();
            for kv in kvs.iter() {
                let suffix = &kv.key()[prefix_len..];
                if let Ok(s) = std::str::from_utf8(suffix) {
                    ids.push(s.to_string());
                }
            }
            Ok(ids)
        })
            ;
        let id_strs = id_strs.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("ssh key index uuid: {e}")))?;
            let by_id_key = keys::ssh_key_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let key: SshKey = serde_json::from_slice(&bytes)
                    .map_err(de_err("ssh key"))?;
                out.push(key);
            }
        }
        Ok(out)
    }
}
