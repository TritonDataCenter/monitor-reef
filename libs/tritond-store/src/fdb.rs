// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FoundationDB-backed [`Store`] implementation.
//!
//! Compiled in only when the `foundationdb` cargo feature is enabled,
//! because linking pulls in `libfdb_c.so` (FoundationDB client
//! library, currently 7.3.x). Default builds don't need FDB installed
//! and use [`crate::MemStore`] instead.
//!
//! # Boot semantics
//!
//! The FDB Rust binding requires exactly one `boot()` call per
//! process; the returned `NetworkAutoStop` guard must outlive every
//! `Database` handle. We satisfy this with a `OnceLock` plus a
//! `mem::forget` so the network thread runs until the process exits,
//! which is the right shape for a long-running daemon.
//!
//! # Schema
//!
//! Phase 0 lays down the following key prefixes:
//!
//! ```text
//! silo/by_id/<uuid>                 -> JSON-encoded Silo
//! silo/by_name/<name>               -> uuid hyphenated bytes
//! user/by_id/<uuid>                 -> JSON-encoded User
//! user/by_name/<username>           -> uuid hyphenated bytes
//! user/by_federation/<silo>/<sha256>-> uuid hyphenated bytes
//! apikey/by_id/<uuid>               -> JSON-encoded ApiKey
//! apikey/by_lookup/<lookup_id>      -> uuid hyphenated bytes
//! apikey/by_user/<uuid>/<key-uuid>  -> empty (membership index)
//! idp/by_silo/<uuid>                -> JSON-encoded IdpConfig
//! project/by_id/<uuid>              -> JSON-encoded Project
//! project/by_silo/<silo>/<name>     -> uuid hyphenated bytes
//! project/in_silo/<silo>/<proj>     -> empty (membership index)
//! vpc/by_id/<uuid>                  -> JSON-encoded Vpc
//! vpc/by_project/<proj>/<name>      -> uuid hyphenated bytes
//! vpc/in_project/<proj>/<vpc>       -> empty (membership index)
//! vpc/by_vni/<vni-hex8>             -> uuid hyphenated bytes
//! subnet/by_id/<uuid>               -> JSON-encoded Subnet
//! subnet/by_vpc/<vpc>/<name>        -> uuid hyphenated bytes
//! subnet/in_vpc/<vpc>/<subnet>      -> empty (membership index)
//! ssh_key/by_id/<uuid>              -> JSON-encoded SshKey
//! ssh_key/by_silo/<silo>/<name>     -> uuid hyphenated bytes
//! ssh_key/by_fingerprint/<silo>/<sha256-hex>
//!                                   -> uuid hyphenated bytes
//! ssh_key/in_silo/<silo>/<key>      -> empty (membership index)
//! image/by_id/<uuid>                -> JSON-encoded Image
//! image/by_silo/<silo>/<name>       -> uuid hyphenated bytes
//! image/in_silo/<silo>/<image>      -> empty (membership index)
//! quota/by_project/<project>        -> JSON-encoded Quota
//! instance/by_id/<uuid>             -> JSON-encoded Instance
//! instance/by_project/<proj>/<name> -> uuid hyphenated bytes
//! instance/in_project/<proj>/<inst> -> empty (membership index)
//! nic/by_id/<uuid>                  -> JSON-encoded Nic
//! nic/in_instance/<inst>/<nic>      -> empty (membership index)
//! nic/ip_alloc/<subnet>/v4/<addr>   -> empty (allocation index, v4)
//! nic/ip_alloc/<subnet>/v6/<addr>   -> empty (allocation index, v6)
//! disk/by_id/<uuid>                 -> JSON-encoded Disk
//! disk/in_instance/<inst>/<disk>    -> empty (membership index)
//! job/by_id/<uuid>                  -> JSON-encoded ProvisioningJob
//! job/pending/<seq-be-u64>          -> uuid hyphenated bytes (FIFO queue)
//! job/seq/counter                   -> next seq, big-endian u64
//! system/<tag>                      -> raw bytes (e.g. JWT signing key)
//! ```
//!
//! Each multi-key write happens in a single transaction so name
//! uniqueness and index consistency are enforced atomically.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use foundationdb::{Database, FdbBindingError, KeySelector, RangeOption};
use rand::Rng;
use uuid::Uuid;

use crate::{
    ApiKey, Disk, DiskKind, IdpConfig, Image, Instance, InstanceCreateResult, JobOutcome,
    JobStatus, JobStatusKind, LifecycleState, LifecycleStateKind, NewImage, NewInstance, NewJob,
    NewProject, NewQuota, NewSilo, NewSshKey, NewSubnet, NewVpc, Nic, Project, ProvisioningJob,
    Quota, Silo, SshKey, Store, StoreError, Subnet, SystemKey, User, VPC_VNI_MAX,
    VPC_VNI_RESERVED_CEILING, Vpc,
};

/// Maximum attempts to draw a fresh VNI before giving up. Mirrors the
/// in-memory store's cap; with ~16.7M candidates this is operationally
/// unreachable.
const VNI_RETRY_ATTEMPTS: usize = 8;

static FDB_NETWORK: OnceLock<()> = OnceLock::new();

/// Boot the FDB network thread (idempotent). The returned guard is
/// intentionally leaked so FDB stays alive for the rest of the
/// process.
fn ensure_fdb_booted() {
    FDB_NETWORK.get_or_init(|| {
        // SAFETY: boot() must be called at most once per process. The
        // OnceLock guarantees that. The returned guard is leaked so it
        // outlives all Database instances, which is the requirement.
        let guard = unsafe { foundationdb::boot() };
        std::mem::forget(guard);
    });
}

/// FoundationDB-backed [`Store`].
pub struct FdbStore {
    db: Arc<Database>,
}

impl FdbStore {
    /// Open the database described by `cluster_file_path`. Pass `None`
    /// to use FoundationDB's default cluster file resolution
    /// (`FDB_CLUSTER_FILE` env, `/etc/foundationdb/fdb.cluster`).
    pub fn open(cluster_file_path: Option<&str>) -> Result<Self, StoreError> {
        ensure_fdb_booted();
        let db = Database::new(cluster_file_path)
            .map_err(|e| StoreError::Backend(format!("open FDB cluster: {e}")))?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Get a shared handle to the underlying [`foundationdb::Database`].
    /// Used by adjacent crates (e.g. `tritond-audit::FdbChain`) that
    /// need to share the boot-once FDB network thread without opening
    /// a separate `Database` handle.
    pub fn database(&self) -> Arc<Database> {
        Arc::clone(&self.db)
    }

    fn silo_by_id_key(id: Uuid) -> Vec<u8> {
        format!("silo/by_id/{id}").into_bytes()
    }

    fn silo_by_name_key(name: &str) -> Vec<u8> {
        format!("silo/by_name/{name}").into_bytes()
    }

    fn user_by_id_key(id: Uuid) -> Vec<u8> {
        format!("user/by_id/{id}").into_bytes()
    }

    fn user_by_name_key(name: &str) -> Vec<u8> {
        format!("user/by_name/{name}").into_bytes()
    }

    fn user_prefix() -> &'static [u8] {
        b"user/by_id/"
    }

    fn apikey_by_id_key(id: Uuid) -> Vec<u8> {
        format!("apikey/by_id/{id}").into_bytes()
    }

    fn apikey_by_lookup_key(lookup_id: &str) -> Vec<u8> {
        format!("apikey/by_lookup/{lookup_id}").into_bytes()
    }

    fn apikey_user_index_key(user_id: Uuid, key_id: Uuid) -> Vec<u8> {
        format!("apikey/by_user/{user_id}/{key_id}").into_bytes()
    }

    fn apikey_user_index_prefix(user_id: Uuid) -> Vec<u8> {
        format!("apikey/by_user/{user_id}/").into_bytes()
    }

    fn system_key(key: SystemKey) -> Vec<u8> {
        format!("system/{}", key.tag()).into_bytes()
    }

    fn user_federation_key(silo_id: Uuid, issuer: &str, subject: &str) -> Vec<u8> {
        // SHA-256 of `issuer\0subject` → fixed-length, no escaping
        // worries for arbitrary issuer URLs that contain slashes.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(issuer.as_bytes());
        hasher.update(b"\0");
        hasher.update(subject.as_bytes());
        let digest = hasher.finalize();
        let hex = digest_to_hex(&digest);
        format!("user/by_federation/{silo_id}/{hex}").into_bytes()
    }

    fn idp_config_key(silo_id: Uuid) -> Vec<u8> {
        format!("idp/by_silo/{silo_id}").into_bytes()
    }

    fn idp_config_prefix() -> &'static [u8] {
        b"idp/by_silo/"
    }

    fn project_by_id_key(id: Uuid) -> Vec<u8> {
        format!("project/by_id/{id}").into_bytes()
    }

    fn project_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
        format!("project/by_silo/{silo_id}/{name}").into_bytes()
    }

    fn project_in_silo_key(silo_id: Uuid, project_id: Uuid) -> Vec<u8> {
        format!("project/in_silo/{silo_id}/{project_id}").into_bytes()
    }

    fn project_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
        format!("project/in_silo/{silo_id}/").into_bytes()
    }

    fn vpc_by_id_key(id: Uuid) -> Vec<u8> {
        format!("vpc/by_id/{id}").into_bytes()
    }

    fn vpc_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
        format!("vpc/by_project/{project_id}/{name}").into_bytes()
    }

    fn vpc_in_project_key(project_id: Uuid, vpc_id: Uuid) -> Vec<u8> {
        format!("vpc/in_project/{project_id}/{vpc_id}").into_bytes()
    }

    fn vpc_in_project_prefix(project_id: Uuid) -> Vec<u8> {
        format!("vpc/in_project/{project_id}/").into_bytes()
    }

    fn vpc_by_vni_key(vni: u32) -> Vec<u8> {
        format!("vpc/by_vni/{vni:08x}").into_bytes()
    }

    fn subnet_by_id_key(id: Uuid) -> Vec<u8> {
        format!("subnet/by_id/{id}").into_bytes()
    }

    fn subnet_by_vpc_name_key(vpc_id: Uuid, name: &str) -> Vec<u8> {
        format!("subnet/by_vpc/{vpc_id}/{name}").into_bytes()
    }

    fn subnet_in_vpc_key(vpc_id: Uuid, subnet_id: Uuid) -> Vec<u8> {
        format!("subnet/in_vpc/{vpc_id}/{subnet_id}").into_bytes()
    }

    fn subnet_in_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
        format!("subnet/in_vpc/{vpc_id}/").into_bytes()
    }

    fn ssh_key_by_id_key(id: Uuid) -> Vec<u8> {
        format!("ssh_key/by_id/{id}").into_bytes()
    }

    fn ssh_key_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
        format!("ssh_key/by_silo/{silo_id}/{name}").into_bytes()
    }

    fn ssh_key_by_fingerprint_key(silo_id: Uuid, fingerprint: &str) -> Vec<u8> {
        format!("ssh_key/by_fingerprint/{silo_id}/{fingerprint}").into_bytes()
    }

    fn ssh_key_in_silo_key(silo_id: Uuid, key_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_silo/{silo_id}/{key_id}").into_bytes()
    }

    fn ssh_key_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_silo/{silo_id}/").into_bytes()
    }

    fn image_by_id_key(id: Uuid) -> Vec<u8> {
        format!("image/by_id/{id}").into_bytes()
    }

    fn image_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
        format!("image/by_silo/{silo_id}/{name}").into_bytes()
    }

    fn image_in_silo_key(silo_id: Uuid, image_id: Uuid) -> Vec<u8> {
        format!("image/in_silo/{silo_id}/{image_id}").into_bytes()
    }

    fn image_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
        format!("image/in_silo/{silo_id}/").into_bytes()
    }

    fn quota_by_project_key(project_id: Uuid) -> Vec<u8> {
        format!("quota/by_project/{project_id}").into_bytes()
    }

    fn instance_by_id_key(id: Uuid) -> Vec<u8> {
        format!("instance/by_id/{id}").into_bytes()
    }

    fn instance_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
        format!("instance/by_project/{project_id}/{name}").into_bytes()
    }

    fn instance_in_project_key(project_id: Uuid, instance_id: Uuid) -> Vec<u8> {
        format!("instance/in_project/{project_id}/{instance_id}").into_bytes()
    }

    fn instance_in_project_prefix(project_id: Uuid) -> Vec<u8> {
        format!("instance/in_project/{project_id}/").into_bytes()
    }

    fn nic_by_id_key(id: Uuid) -> Vec<u8> {
        format!("nic/by_id/{id}").into_bytes()
    }

    fn nic_in_instance_key(instance_id: Uuid, nic_id: Uuid) -> Vec<u8> {
        format!("nic/in_instance/{instance_id}/{nic_id}").into_bytes()
    }

    fn nic_in_instance_prefix(instance_id: Uuid) -> Vec<u8> {
        format!("nic/in_instance/{instance_id}/").into_bytes()
    }

    fn nic_ip_alloc_v4_key(subnet_id: Uuid, ip: std::net::Ipv4Addr) -> Vec<u8> {
        format!("nic/ip_alloc/{subnet_id}/v4/{ip}").into_bytes()
    }

    fn nic_ip_alloc_v6_key(subnet_id: Uuid, ip: std::net::Ipv6Addr) -> Vec<u8> {
        format!("nic/ip_alloc/{subnet_id}/v6/{ip}").into_bytes()
    }

    fn nic_ip_alloc_v4_prefix(subnet_id: Uuid) -> Vec<u8> {
        format!("nic/ip_alloc/{subnet_id}/v4/").into_bytes()
    }

    fn nic_ip_alloc_v6_prefix(subnet_id: Uuid) -> Vec<u8> {
        format!("nic/ip_alloc/{subnet_id}/v6/").into_bytes()
    }

    fn disk_by_id_key(id: Uuid) -> Vec<u8> {
        format!("disk/by_id/{id}").into_bytes()
    }

    fn disk_in_instance_key(instance_id: Uuid, disk_id: Uuid) -> Vec<u8> {
        format!("disk/in_instance/{instance_id}/{disk_id}").into_bytes()
    }

    fn disk_in_instance_prefix(instance_id: Uuid) -> Vec<u8> {
        format!("disk/in_instance/{instance_id}/").into_bytes()
    }

    fn job_by_id_key(id: Uuid) -> Vec<u8> {
        format!("job/by_id/{id}").into_bytes()
    }

    fn job_pending_key(seq: u64) -> Vec<u8> {
        // 16-char zero-padded hex so the FDB key sort matches the
        // numeric u64 sort. (Big-endian raw bytes would also work,
        // but the prefix `job/pending/` is utf8 so we stay readable.)
        format!("job/pending/{seq:016x}").into_bytes()
    }

    fn job_pending_prefix() -> &'static [u8] {
        b"job/pending/"
    }

    fn job_seq_counter_key() -> &'static [u8] {
        b"job/seq/counter"
    }
}

fn digest_to_hex(digest: &[u8]) -> String {
    static HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}

/// Outcome carried out of a transaction closure when the conflict
/// reason is one of *our* invariants (e.g. duplicate name) rather
/// than an FDB-level retryable error.
enum CreateOutcome {
    Created,
    NameTaken,
}

/// Outcome of an api-key delete transaction.
enum DeleteOutcome {
    Deleted,
    NotFound,
}

/// Compute the half-open range `[prefix, prefix++)` for prefix scans.
fn prefix_range(prefix: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut end = prefix.to_vec();
    // Increment the last byte to get the exclusive upper bound. If
    // every byte were 0xFF the prefix would be at the end of the
    // keyspace, but our key bytes are ASCII so that case never arises.
    for byte in end.iter_mut().rev() {
        if *byte < 0xFF {
            *byte += 1;
            return (prefix.to_vec(), end);
        }
        *byte = 0;
    }
    // Fallthrough: append a 0 byte to widen the range.
    end.push(0);
    (prefix.to_vec(), end)
}

#[async_trait]
impl Store for FdbStore {
    async fn create_silo(&self, req: NewSilo) -> Result<Silo, StoreError> {
        let silo = Silo {
            id: Uuid::new_v4(),
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&silo)
            .map_err(|e| StoreError::Backend(format!("serialize silo: {e}")))?;
        let by_id_key = Self::silo_by_id_key(silo.id);
        let by_name_key = Self::silo_by_name_key(&silo.name);
        let id_str = silo.id.to_string();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(CreateOutcome::NameTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    Ok(CreateOutcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(CreateOutcome::Created) => Ok(silo),
            Ok(CreateOutcome::NameTaken) => Err(StoreError::Conflict(format!(
                "silo with name {:?} already exists",
                silo.name
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_silo(&self, id: Uuid) -> Result<Silo, StoreError> {
        let key = Self::silo_by_id_key(id);
        let bytes = self.read_bytes(&key).await?;
        match bytes {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Backend(format!("deserialize silo: {e}"))),
            None => Err(StoreError::NotFound),
        }
    }

    async fn create_user(&self, user: User) -> Result<User, StoreError> {
        let value = serde_json::to_vec(&user)
            .map_err(|e| StoreError::Backend(format!("serialize user: {e}")))?;
        let by_id_key = Self::user_by_id_key(user.id);
        let by_name_key = Self::user_by_name_key(&user.username);
        let federation_key = match (user.silo_id, user.federation.as_ref()) {
            (Some(silo_id), Some(fed)) => Some(Self::user_federation_key(
                silo_id,
                &fed.issuer,
                &fed.subject,
            )),
            _ => None,
        };
        let id_str = user.id.to_string();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let federation_key = federation_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(CreateOutcome::NameTaken);
                    }
                    if let Some(fk) = federation_key.as_deref()
                        && tr.get(fk, false).await?.is_some()
                    {
                        return Ok(CreateOutcome::NameTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    if let Some(fk) = federation_key.as_deref() {
                        tr.set(fk, &id_bytes);
                    }
                    Ok(CreateOutcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(CreateOutcome::Created) => Ok(user),
            Ok(CreateOutcome::NameTaken) => Err(StoreError::Conflict(format!(
                "user with username {:?} or federation triple already exists",
                user.username
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_user_by_username(&self, username: &str) -> Result<User, StoreError> {
        let by_name_key = Self::user_by_name_key(username);
        let id_bytes = self
            .read_bytes(&by_name_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("user index value not utf8: {e}")))?;
        let id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("user index value not uuid: {e}")))?;
        self.get_user_by_id(id).await
    }

    async fn get_user_by_id(&self, id: Uuid) -> Result<User, StoreError> {
        let key = Self::user_by_id_key(id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize user: {e}")))
    }

    async fn has_any_user(&self) -> Result<bool, StoreError> {
        let (begin, end) = prefix_range(Self::user_prefix());
        let result: Result<bool, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        limit: Some(1),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok(kvs.iter().next().is_some())
                }
            })
            .await;
        result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))
    }

    async fn create_api_key(&self, key: ApiKey) -> Result<ApiKey, StoreError> {
        let value = serde_json::to_vec(&key)
            .map_err(|e| StoreError::Backend(format!("serialize api key: {e}")))?;
        let by_id_key = Self::apikey_by_id_key(key.id);
        let by_lookup_key = Self::apikey_by_lookup_key(&key.lookup_id);
        let user_index_key = Self::apikey_user_index_key(key.user_id, key.id);
        let id_str = key.id.to_string();
        let lookup_id = key.lookup_id.clone();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_lookup_key = by_lookup_key.clone();
                let user_index_key = user_index_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&by_lookup_key, false).await?.is_some() {
                        return Ok(CreateOutcome::NameTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_lookup_key, &id_bytes);
                    tr.set(&user_index_key, b"");
                    Ok(CreateOutcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(CreateOutcome::Created) => Ok(key),
            Ok(CreateOutcome::NameTaken) => Err(StoreError::Conflict(format!(
                "api key with lookup id {lookup_id:?} already exists"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn list_api_keys(&self, user_id: Uuid) -> Result<Vec<ApiKey>, StoreError> {
        let prefix = Self::apikey_user_index_prefix(user_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        // Collect the key ids that this user owns.
        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("api key index uuid: {e}")))?;
            let by_id_key = Self::apikey_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let key: ApiKey = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize api key: {e}")))?;
                out.push(key);
            }
        }
        Ok(out)
    }

    async fn get_api_key_by_lookup_id(&self, lookup_id: &str) -> Result<ApiKey, StoreError> {
        let by_lookup_key = Self::apikey_by_lookup_key(lookup_id);
        let id_bytes = self
            .read_bytes(&by_lookup_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("api key lookup index not utf8: {e}")))?;
        let id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("api key lookup index not uuid: {e}")))?;
        let by_id_key = Self::apikey_by_id_key(id);
        let bytes = self
            .read_bytes(&by_id_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize api key: {e}")))
    }

    async fn delete_api_key(&self, user_id: Uuid, key_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = Self::apikey_by_id_key(key_id);
        let user_index_key = Self::apikey_user_index_key(user_id, key_id);

        // We need the lookup_id to clear the by_lookup index. Read
        // the record outside the transaction; the rare race where it
        // was concurrently deleted resolves to NotFound below.
        let record_bytes = match self.read_bytes(&by_id_key).await? {
            Some(bytes) => bytes,
            None => return Err(StoreError::NotFound),
        };
        let record: ApiKey = serde_json::from_slice(&record_bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize api key: {e}")))?;
        if record.user_id != user_id {
            return Err(StoreError::NotFound);
        }
        let by_lookup_key = Self::apikey_by_lookup_key(&record.lookup_id);

        let outcome: Result<DeleteOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_lookup_key = by_lookup_key.clone();
                let user_index_key = user_index_key.clone();
                async move {
                    // The user-index entry is the source of truth for
                    // ownership; if it's gone, somebody already
                    // deleted the key.
                    if tr.get(&user_index_key, false).await?.is_none() {
                        return Ok(DeleteOutcome::NotFound);
                    }
                    tr.clear(&by_id_key);
                    tr.clear(&by_lookup_key);
                    tr.clear(&user_index_key);
                    Ok(DeleteOutcome::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DeleteOutcome::Deleted) => Ok(()),
            Ok(DeleteOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_system_key(&self, key: SystemKey) -> Result<Vec<u8>, StoreError> {
        let storage_key = Self::system_key(key);
        self.read_bytes(&storage_key)
            .await?
            .ok_or(StoreError::NotFound)
    }

    async fn put_system_key(&self, key: SystemKey, value: Vec<u8>) -> Result<(), StoreError> {
        let storage_key = Self::system_key(key);
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let storage_key = storage_key.clone();
                let value = value.clone();
                async move {
                    tr.set(&storage_key, &value);
                    Ok(())
                }
            })
            .await;
        result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))
    }

    async fn get_user_by_federation(
        &self,
        silo_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<User, StoreError> {
        let federation_key = Self::user_federation_key(silo_id, issuer, subject);
        let id_bytes = self
            .read_bytes(&federation_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("federation index value not utf8: {e}")))?;
        let id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("federation index value not uuid: {e}")))?;
        self.get_user_by_id(id).await
    }

    async fn put_idp_config(
        &self,
        silo_id: Uuid,
        config: IdpConfig,
    ) -> Result<IdpConfig, StoreError> {
        let key = Self::idp_config_key(silo_id);
        let value = serde_json::to_vec(&config)
            .map_err(|e| StoreError::Backend(format!("serialize idp config: {e}")))?;
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let value = value.clone();
                async move {
                    tr.set(&key, &value);
                    Ok(())
                }
            })
            .await;
        result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;
        Ok(config)
    }

    async fn get_idp_config(&self, silo_id: Uuid) -> Result<IdpConfig, StoreError> {
        let key = Self::idp_config_key(silo_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize idp config: {e}")))
    }

    async fn delete_idp_config(&self, silo_id: Uuid) -> Result<(), StoreError> {
        let key = Self::idp_config_key(silo_id);
        let outcome: Result<DeleteOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move {
                    if tr.get(&key, false).await?.is_none() {
                        return Ok(DeleteOutcome::NotFound);
                    }
                    tr.clear(&key);
                    Ok(DeleteOutcome::Deleted)
                }
            })
            .await;
        match outcome {
            Ok(DeleteOutcome::Deleted) => Ok(()),
            Ok(DeleteOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn list_idp_configs(&self) -> Result<Vec<(Uuid, IdpConfig)>, StoreError> {
        let (begin, end) = prefix_range(Self::idp_config_prefix());
        let prefix_len = Self::idp_config_prefix().len();

        let result: Result<Vec<(Vec<u8>, Vec<u8>)>, FdbBindingError> = self
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
                    Ok(kvs
                        .iter()
                        .map(|kv| (kv.key().to_vec(), kv.value().to_vec()))
                        .collect())
                }
            })
            .await;
        let raws = result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;
        let mut out = Vec::with_capacity(raws.len());
        for (key, value) in raws {
            let suffix = &key[prefix_len..];
            let silo_str = std::str::from_utf8(suffix)
                .map_err(|e| StoreError::Backend(format!("idp index key not utf8: {e}")))?;
            let silo_id = Uuid::parse_str(silo_str)
                .map_err(|e| StoreError::Backend(format!("idp index key not uuid: {e}")))?;
            let config: IdpConfig = serde_json::from_slice(&value)
                .map_err(|e| StoreError::Backend(format!("deserialize idp config: {e}")))?;
            out.push((silo_id, config));
        }
        Ok(out)
    }

    async fn create_project(&self, silo_id: Uuid, req: NewProject) -> Result<Project, StoreError> {
        let project = Project {
            id: Uuid::new_v4(),
            silo_id,
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&project)
            .map_err(|e| StoreError::Backend(format!("serialize project: {e}")))?;
        let by_id_key = Self::project_by_id_key(project.id);
        let by_name_key = Self::project_by_silo_name_key(silo_id, &project.name);
        let in_silo_key = Self::project_in_silo_key(silo_id, project.id);
        let silo_check_key = Self::silo_by_id_key(silo_id);
        let id_str = project.id.to_string();
        let name_str = project.name.clone();

        // Outcome distinguishes silo-missing from name-conflict so the
        // single transaction can convey both into our caller's error
        // shape.
        enum Outcome {
            Created,
            SiloMissing,
            NameTaken,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_silo_key = in_silo_key.clone();
                let silo_check_key = silo_check_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&silo_check_key, false).await?.is_none() {
                        return Ok(Outcome::SiloMissing);
                    }
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_silo_key, b"");
                    Ok(Outcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created) => Ok(project),
            Ok(Outcome::SiloMissing) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "project with name {name_str:?} already exists in silo {silo_id}"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_project(&self, project_id: Uuid) -> Result<Project, StoreError> {
        let key = Self::project_by_id_key(project_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize project: {e}")))
    }

    async fn list_projects_in_silo(&self, silo_id: Uuid) -> Result<Vec<Project>, StoreError> {
        let prefix = Self::project_in_silo_prefix(silo_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("project index uuid: {e}")))?;
            let by_id_key = Self::project_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let project: Project = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize project: {e}")))?;
                out.push(project);
            }
        }
        Ok(out)
    }

    async fn delete_project(&self, project_id: Uuid) -> Result<(), StoreError> {
        // Read the row outside the transaction so we know the
        // silo_id + name to clear from the indices. Concurrent
        // delete shows up as Outcome::Vanished below.
        let by_id_key = Self::project_by_id_key(project_id);
        let bytes = match self.read_bytes(&by_id_key).await? {
            Some(b) => b,
            None => return Err(StoreError::NotFound),
        };
        let project: Project = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize project: {e}")))?;
        let by_name_key = Self::project_by_silo_name_key(project.silo_id, &project.name);
        let in_silo_key = Self::project_in_silo_key(project.silo_id, project.id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_silo_key = in_silo_key.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DelOut::Vanished);
                    }
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_silo_key);
                    Ok(DelOut::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DelOut::Deleted) => Ok(()),
            Ok(DelOut::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_vpc(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewVpc,
    ) -> Result<Vpc, StoreError> {
        // Outcome distinguishes our four invariant failures from FDB
        // transport errors. VniTaken triggers a retry at this layer
        // (a fresh draw + new transaction); the others surface to the
        // caller verbatim.
        enum Outcome {
            Created(Vpc),
            ProjectMissingOrWrongSilo,
            NameTaken,
            VniTaken,
        }

        let project_check_key = Self::project_by_id_key(project_id);
        let by_name_key = Self::vpc_by_project_name_key(project_id, &req.name);

        for _ in 0..VNI_RETRY_ATTEMPTS {
            let vni = rand::rng().random_range(VPC_VNI_RESERVED_CEILING..VPC_VNI_MAX);
            let candidate = Vpc {
                id: Uuid::new_v4(),
                silo_id,
                project_id,
                name: req.name.clone(),
                description: req.description.clone().unwrap_or_default(),
                vni,
                ipv4_block: req.ipv4_block,
                ipv6_block: req.ipv6_block,
                created_at: Utc::now(),
            };
            let value = serde_json::to_vec(&candidate)
                .map_err(|e| StoreError::Backend(format!("serialize vpc: {e}")))?;
            let by_id_key = Self::vpc_by_id_key(candidate.id);
            let in_project_key = Self::vpc_in_project_key(project_id, candidate.id);
            let by_vni_key = Self::vpc_by_vni_key(vni);
            let id_str = candidate.id.to_string();

            let outcome: Result<Outcome, FdbBindingError> = self
                .db
                .run(|tr, _| {
                    let project_check_key = project_check_key.clone();
                    let by_id_key = by_id_key.clone();
                    let by_name_key = by_name_key.clone();
                    let in_project_key = in_project_key.clone();
                    let by_vni_key = by_vni_key.clone();
                    let value = value.clone();
                    let id_bytes = id_str.as_bytes().to_vec();
                    let candidate = candidate.clone();
                    async move {
                        // Project must exist and live in the silo the
                        // caller claims. Silo mismatch surfaces as
                        // NotFound (project is invisible to a foreign
                        // silo).
                        let project_bytes = match tr.get(&project_check_key, false).await? {
                            Some(b) => b,
                            None => return Ok(Outcome::ProjectMissingOrWrongSilo),
                        };
                        let project: Project = match serde_json::from_slice(&project_bytes) {
                            Ok(p) => p,
                            Err(_) => return Ok(Outcome::ProjectMissingOrWrongSilo),
                        };
                        if project.silo_id != silo_id {
                            return Ok(Outcome::ProjectMissingOrWrongSilo);
                        }
                        if tr.get(&by_name_key, false).await?.is_some() {
                            return Ok(Outcome::NameTaken);
                        }
                        if tr.get(&by_vni_key, false).await?.is_some() {
                            return Ok(Outcome::VniTaken);
                        }
                        tr.set(&by_id_key, &value);
                        tr.set(&by_name_key, &id_bytes);
                        tr.set(&in_project_key, b"");
                        tr.set(&by_vni_key, &id_bytes);
                        Ok(Outcome::Created(candidate))
                    }
                })
                .await;

            match outcome {
                Ok(Outcome::Created(vpc)) => return Ok(vpc),
                Ok(Outcome::ProjectMissingOrWrongSilo) => return Err(StoreError::NotFound),
                Ok(Outcome::NameTaken) => {
                    return Err(StoreError::Conflict(format!(
                        "vpc with name {:?} already exists in project {project_id}",
                        req.name
                    )));
                }
                Ok(Outcome::VniTaken) => continue,
                Err(e) => return Err(StoreError::Backend(format!("FDB transaction: {e}"))),
            }
        }

        Err(StoreError::Backend(format!(
            "VNI exhausted after {VNI_RETRY_ATTEMPTS} retries"
        )))
    }

    async fn get_vpc(&self, vpc_id: Uuid) -> Result<Vpc, StoreError> {
        let key = Self::vpc_by_id_key(vpc_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize vpc: {e}")))
    }

    async fn list_vpcs_in_project(&self, project_id: Uuid) -> Result<Vec<Vpc>, StoreError> {
        let prefix = Self::vpc_in_project_prefix(project_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("vpc index uuid: {e}")))?;
            let by_id_key = Self::vpc_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let vpc: Vpc = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize vpc: {e}")))?;
                out.push(vpc);
            }
        }
        Ok(out)
    }

    async fn delete_vpc(&self, vpc_id: Uuid) -> Result<(), StoreError> {
        // Read the row outside the transaction so we know project_id +
        // name + vni for the index clears.
        let by_id_key = Self::vpc_by_id_key(vpc_id);
        let bytes = match self.read_bytes(&by_id_key).await? {
            Some(b) => b,
            None => return Err(StoreError::NotFound),
        };
        let vpc: Vpc = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize vpc: {e}")))?;
        let by_name_key = Self::vpc_by_project_name_key(vpc.project_id, &vpc.name);
        let in_project_key = Self::vpc_in_project_key(vpc.project_id, vpc.id);
        let by_vni_key = Self::vpc_by_vni_key(vpc.vni);
        let subnet_prefix = Self::subnet_in_vpc_prefix(vpc_id);
        let (subnet_begin, subnet_end) = prefix_range(&subnet_prefix);

        enum DelOut {
            Deleted,
            Vanished,
            HasSubnets,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_project_key = in_project_key.clone();
                let by_vni_key = by_vni_key.clone();
                let subnet_begin = subnet_begin.clone();
                let subnet_end = subnet_end.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DelOut::Vanished);
                    }
                    // Refuse the delete if any subnet still references
                    // this VPC. Operator must drop subnets first; no
                    // cascade in Phase 0.
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(subnet_begin),
                        end: KeySelector::first_greater_or_equal(subnet_end),
                        limit: Some(1),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    if kvs.iter().next().is_some() {
                        return Ok(DelOut::HasSubnets);
                    }
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_project_key);
                    tr.clear(&by_vni_key);
                    Ok(DelOut::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DelOut::Deleted) => Ok(()),
            Ok(DelOut::Vanished) => Err(StoreError::NotFound),
            Ok(DelOut::HasSubnets) => Err(StoreError::Conflict(format!(
                "vpc {vpc_id} still has subnets attached; delete subnets first"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_subnet(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewSubnet,
    ) -> Result<Subnet, StoreError> {
        let vpc_check_key = Self::vpc_by_id_key(vpc_id);
        let subnet_prefix = Self::subnet_in_vpc_prefix(vpc_id);
        let (peer_begin, peer_end) = prefix_range(&subnet_prefix);

        // The candidate is finalized inside the transaction so
        // `created_at` and the new uuid are stable across the run.
        let candidate_id = Uuid::new_v4();
        let by_id_key = Self::subnet_by_id_key(candidate_id);
        let by_name_key = Self::subnet_by_vpc_name_key(vpc_id, &req.name);
        let in_vpc_key = Self::subnet_in_vpc_key(vpc_id, candidate_id);
        let id_str = candidate_id.to_string();

        enum Outcome {
            Created(Subnet),
            VpcMissingOrWrongParent,
            CidrViolation(String),
            NameTaken,
        }

        let req_for_txn = req.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let vpc_check_key = vpc_check_key.clone();
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_vpc_key = in_vpc_key.clone();
                let peer_begin = peer_begin.clone();
                let peer_end = peer_end.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                let req = req_for_txn.clone();
                async move {
                    // VPC parent: must exist + silo + project match.
                    let vpc_bytes = match tr.get(&vpc_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::VpcMissingOrWrongParent),
                    };
                    let vpc: Vpc = match serde_json::from_slice(&vpc_bytes) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::VpcMissingOrWrongParent),
                    };
                    if vpc.silo_id != silo_id || vpc.project_id != project_id {
                        return Ok(Outcome::VpcMissingOrWrongParent);
                    }

                    // Name uniqueness within VPC.
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }

                    // Read peer subnets for CIDR overlap validation.
                    // Two passes: first the in_vpc index, then each
                    // by_id record (the index value carries no CIDR).
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(peer_begin),
                        end: KeySelector::first_greater_or_equal(peer_end),
                        ..RangeOption::default()
                    };
                    let peer_index = tr.get_range(&opt, 1, false).await?;
                    // Collect peer ids first so the FDB iterator
                    // (which holds non-Send raw pointers) doesn't
                    // straddle the per-peer `tr.get` await below.
                    let suffix_start = subnet_prefix_len(vpc_id);
                    let mut peer_ids: Vec<Uuid> = Vec::new();
                    for kv in peer_index.iter() {
                        let suffix = &kv.key()[suffix_start..];
                        if let Ok(s) = std::str::from_utf8(suffix)
                            && let Ok(id) = Uuid::parse_str(s)
                        {
                            peer_ids.push(id);
                        }
                    }
                    drop(peer_index);
                    let mut peers: Vec<Subnet> = Vec::with_capacity(peer_ids.len());
                    for peer_id in peer_ids {
                        let peer_key = format!("subnet/by_id/{peer_id}").into_bytes();
                        let peer_bytes = match tr.get(&peer_key, false).await? {
                            Some(b) => b,
                            None => continue,
                        };
                        if let Ok(peer) = serde_json::from_slice::<Subnet>(&peer_bytes) {
                            peers.push(peer);
                        }
                    }
                    if let Err(msg) = crate::types::validate_subnet_cidrs(
                        &vpc,
                        req.ipv4_block,
                        req.ipv6_block,
                        &peers,
                    ) {
                        return Ok(Outcome::CidrViolation(msg));
                    }

                    let candidate = Subnet {
                        id: candidate_id,
                        silo_id,
                        project_id,
                        vpc_id,
                        name: req.name.clone(),
                        description: req.description.unwrap_or_default(),
                        ipv4_block: req.ipv4_block,
                        ipv6_block: req.ipv6_block,
                        created_at: Utc::now(),
                    };
                    let value = match serde_json::to_vec(&candidate) {
                        Ok(v) => v,
                        Err(_) => {
                            // Treat serialize failure as a generic
                            // backend error; bubble up via the
                            // FdbBindingError channel by returning a
                            // synthetic conflict outcome would be
                            // wrong. We can't easily produce an
                            // FdbBindingError here, so this branch
                            // falls back to the conflict path with a
                            // distinct message.
                            return Ok(Outcome::CidrViolation(
                                "internal serialize error".to_string(),
                            ));
                        }
                    };
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_vpc_key, b"");
                    Ok(Outcome::Created(candidate))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(s)) => Ok(s),
            Ok(Outcome::VpcMissingOrWrongParent) => Err(StoreError::NotFound),
            Ok(Outcome::CidrViolation(msg)) => Err(StoreError::Conflict(msg)),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "subnet with name {:?} already exists in vpc {vpc_id}",
                req.name
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_subnet(&self, subnet_id: Uuid) -> Result<Subnet, StoreError> {
        let key = Self::subnet_by_id_key(subnet_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize subnet: {e}")))
    }

    async fn list_subnets_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<Subnet>, StoreError> {
        let prefix = Self::subnet_in_vpc_prefix(vpc_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("subnet index uuid: {e}")))?;
            let by_id_key = Self::subnet_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let subnet: Subnet = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize subnet: {e}")))?;
                out.push(subnet);
            }
        }
        Ok(out)
    }

    async fn delete_subnet(&self, subnet_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = Self::subnet_by_id_key(subnet_id);
        let bytes = match self.read_bytes(&by_id_key).await? {
            Some(b) => b,
            None => return Err(StoreError::NotFound),
        };
        let subnet: Subnet = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize subnet: {e}")))?;
        let by_name_key = Self::subnet_by_vpc_name_key(subnet.vpc_id, &subnet.name);
        let in_vpc_key = Self::subnet_in_vpc_key(subnet.vpc_id, subnet.id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_vpc_key = in_vpc_key.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DelOut::Vanished);
                    }
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_vpc_key);
                    Ok(DelOut::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DelOut::Deleted) => Ok(()),
            Ok(DelOut::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_ssh_key(
        &self,
        silo_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        let key = SshKey {
            id: Uuid::new_v4(),
            silo_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            public_key: req.public_key,
            fingerprint: fingerprint.clone(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&key)
            .map_err(|e| StoreError::Backend(format!("serialize ssh key: {e}")))?;
        let by_id_key = Self::ssh_key_by_id_key(key.id);
        let by_name_key = Self::ssh_key_by_silo_name_key(silo_id, &key.name);
        let by_fp_key = Self::ssh_key_by_fingerprint_key(silo_id, &key.fingerprint);
        let in_silo_key = Self::ssh_key_in_silo_key(silo_id, key.id);
        let silo_check_key = Self::silo_by_id_key(silo_id);
        let id_str = key.id.to_string();

        enum Outcome {
            Created,
            SiloMissing,
            NameTaken,
            FingerprintTaken,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let by_fp_key = by_fp_key.clone();
                let in_silo_key = in_silo_key.clone();
                let silo_check_key = silo_check_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&silo_check_key, false).await?.is_none() {
                        return Ok(Outcome::SiloMissing);
                    }
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }
                    if tr.get(&by_fp_key, false).await?.is_some() {
                        return Ok(Outcome::FingerprintTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&by_fp_key, &id_bytes);
                    tr.set(&in_silo_key, b"");
                    Ok(Outcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created) => Ok(key),
            Ok(Outcome::SiloMissing) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "ssh key with name {:?} already exists in silo {silo_id}",
                req.name
            ))),
            Ok(Outcome::FingerprintTaken) => Err(StoreError::Conflict(format!(
                "ssh key with fingerprint {fingerprint} already exists in silo {silo_id}"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_ssh_key(&self, key_id: Uuid) -> Result<SshKey, StoreError> {
        let key = Self::ssh_key_by_id_key(key_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize ssh key: {e}")))
    }

    async fn list_ssh_keys_in_silo(&self, silo_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = Self::ssh_key_in_silo_prefix(silo_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("ssh key index uuid: {e}")))?;
            let by_id_key = Self::ssh_key_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let key: SshKey = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize ssh key: {e}")))?;
                out.push(key);
            }
        }
        Ok(out)
    }

    async fn delete_ssh_key(&self, key_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = Self::ssh_key_by_id_key(key_id);
        let bytes = match self.read_bytes(&by_id_key).await? {
            Some(b) => b,
            None => return Err(StoreError::NotFound),
        };
        let key: SshKey = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize ssh key: {e}")))?;
        let by_name_key = Self::ssh_key_by_silo_name_key(key.silo_id, &key.name);
        let by_fp_key = Self::ssh_key_by_fingerprint_key(key.silo_id, &key.fingerprint);
        let in_silo_key = Self::ssh_key_in_silo_key(key.silo_id, key.id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let by_fp_key = by_fp_key.clone();
                let in_silo_key = in_silo_key.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DelOut::Vanished);
                    }
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&by_fp_key);
                    tr.clear(&in_silo_key);
                    Ok(DelOut::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DelOut::Deleted) => Ok(()),
            Ok(DelOut::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_image(&self, silo_id: Uuid, req: NewImage) -> Result<Image, StoreError> {
        let image = Image {
            id: Uuid::new_v4(),
            silo_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            os: req.os,
            version: req.version,
            size_bytes: req.size_bytes,
            sha256: req.sha256,
            source_url: req.source_url,
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&image)
            .map_err(|e| StoreError::Backend(format!("serialize image: {e}")))?;
        let by_id_key = Self::image_by_id_key(image.id);
        let by_name_key = Self::image_by_silo_name_key(silo_id, &image.name);
        let in_silo_key = Self::image_in_silo_key(silo_id, image.id);
        let silo_check_key = Self::silo_by_id_key(silo_id);
        let id_str = image.id.to_string();

        enum Outcome {
            Created,
            SiloMissing,
            NameTaken,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_silo_key = in_silo_key.clone();
                let silo_check_key = silo_check_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&silo_check_key, false).await?.is_none() {
                        return Ok(Outcome::SiloMissing);
                    }
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_silo_key, b"");
                    Ok(Outcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created) => Ok(image),
            Ok(Outcome::SiloMissing) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "image with name {:?} already exists in silo {silo_id}",
                req.name
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_image(&self, image_id: Uuid) -> Result<Image, StoreError> {
        let key = Self::image_by_id_key(image_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize image: {e}")))
    }

    async fn list_images_in_silo(&self, silo_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = Self::image_in_silo_prefix(silo_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("image index uuid: {e}")))?;
            let by_id_key = Self::image_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let image: Image = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize image: {e}")))?;
                out.push(image);
            }
        }
        Ok(out)
    }

    async fn delete_image(&self, image_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = Self::image_by_id_key(image_id);
        let bytes = match self.read_bytes(&by_id_key).await? {
            Some(b) => b,
            None => return Err(StoreError::NotFound),
        };
        let image: Image = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize image: {e}")))?;
        let by_name_key = Self::image_by_silo_name_key(image.silo_id, &image.name);
        let in_silo_key = Self::image_in_silo_key(image.silo_id, image.id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_silo_key = in_silo_key.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DelOut::Vanished);
                    }
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_silo_key);
                    Ok(DelOut::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DelOut::Deleted) => Ok(()),
            Ok(DelOut::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn put_quota(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewQuota,
    ) -> Result<Quota, StoreError> {
        let project_check_key = Self::project_by_id_key(project_id);
        let quota_key = Self::quota_by_project_key(project_id);
        let quota = Quota {
            silo_id,
            project_id,
            cpu_limit: req.cpu_limit,
            memory_bytes: req.memory_bytes,
            disk_bytes: req.disk_bytes,
            instance_limit: req.instance_limit,
            updated_at: Utc::now(),
        };
        let value = serde_json::to_vec(&quota)
            .map_err(|e| StoreError::Backend(format!("serialize quota: {e}")))?;

        enum Outcome {
            Stored,
            ProjectMissingOrWrongSilo,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let project_check_key = project_check_key.clone();
                let quota_key = quota_key.clone();
                let value = value.clone();
                async move {
                    let project_bytes = match tr.get(&project_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::ProjectMissingOrWrongSilo),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongSilo),
                    };
                    if project.silo_id != silo_id {
                        return Ok(Outcome::ProjectMissingOrWrongSilo);
                    }
                    tr.set(&quota_key, &value);
                    Ok(Outcome::Stored)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Stored) => Ok(quota),
            Ok(Outcome::ProjectMissingOrWrongSilo) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_quota(&self, silo_id: Uuid, project_id: Uuid) -> Result<Quota, StoreError> {
        // Read project + quota inside a single transaction so the
        // silo check is consistent with the read.
        let project_check_key = Self::project_by_id_key(project_id);
        let quota_key = Self::quota_by_project_key(project_id);

        enum Outcome {
            Found(Quota),
            ProjectMissingOrWrongSilo,
            QuotaUnset,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let project_check_key = project_check_key.clone();
                let quota_key = quota_key.clone();
                async move {
                    let project_bytes = match tr.get(&project_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::ProjectMissingOrWrongSilo),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongSilo),
                    };
                    if project.silo_id != silo_id {
                        return Ok(Outcome::ProjectMissingOrWrongSilo);
                    }
                    let quota_bytes = match tr.get(&quota_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::QuotaUnset),
                    };
                    let quota: Quota = match serde_json::from_slice(&quota_bytes) {
                        Ok(q) => q,
                        Err(_) => return Ok(Outcome::QuotaUnset),
                    };
                    Ok(Outcome::Found(quota))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Found(q)) => Ok(q),
            Ok(Outcome::ProjectMissingOrWrongSilo) | Ok(Outcome::QuotaUnset) => {
                Err(StoreError::NotFound)
            }
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn delete_quota(&self, silo_id: Uuid, project_id: Uuid) -> Result<(), StoreError> {
        let project_check_key = Self::project_by_id_key(project_id);
        let quota_key = Self::quota_by_project_key(project_id);

        enum Outcome {
            Deleted,
            ProjectMissingOrWrongSilo,
            QuotaUnset,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let project_check_key = project_check_key.clone();
                let quota_key = quota_key.clone();
                async move {
                    let project_bytes = match tr.get(&project_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::ProjectMissingOrWrongSilo),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongSilo),
                    };
                    if project.silo_id != silo_id {
                        return Ok(Outcome::ProjectMissingOrWrongSilo);
                    }
                    if tr.get(&quota_key, false).await?.is_none() {
                        return Ok(Outcome::QuotaUnset);
                    }
                    tr.clear(&quota_key);
                    Ok(Outcome::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Deleted) => Ok(()),
            Ok(Outcome::ProjectMissingOrWrongSilo) | Ok(Outcome::QuotaUnset) => {
                Err(StoreError::NotFound)
            }
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_instance(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewInstance,
    ) -> Result<InstanceCreateResult, StoreError> {
        // All cross-resource reads + the IP allocation set scan +
        // the instance write + the NIC write + the IP-alloc index
        // writes happen in a single transaction. A concurrent
        // delete of any referenced resource aborts cleanly; a
        // concurrent NIC create that would race for the same IP
        // is serialized by FDB's optimistic concurrency.
        let project_check_key = Self::project_by_id_key(project_id);
        let image_check_key = Self::image_by_id_key(req.image_id);
        let subnet_check_key = Self::subnet_by_id_key(req.primary_subnet_id);
        let ssh_key_check_keys: Vec<(Uuid, Vec<u8>)> = req
            .ssh_key_ids
            .iter()
            .map(|id| (*id, Self::ssh_key_by_id_key(*id)))
            .collect();
        let by_name_key = Self::instance_by_project_name_key(project_id, &req.name);
        let alloc_v4_prefix = Self::nic_ip_alloc_v4_prefix(req.primary_subnet_id);
        let alloc_v6_prefix = Self::nic_ip_alloc_v6_prefix(req.primary_subnet_id);
        let (v4_begin, v4_end) = prefix_range(&alloc_v4_prefix);
        let (v6_begin, v6_end) = prefix_range(&alloc_v6_prefix);
        let v4_prefix_len = alloc_v4_prefix.len();
        let v6_prefix_len = alloc_v6_prefix.len();

        let instance_id = Uuid::new_v4();
        let nic_id = Uuid::new_v4();
        let disk_id = Uuid::new_v4();
        let by_id_key = Self::instance_by_id_key(instance_id);
        let in_project_key = Self::instance_in_project_key(project_id, instance_id);
        let nic_by_id_key = Self::nic_by_id_key(nic_id);
        let nic_in_instance_key = Self::nic_in_instance_key(instance_id, nic_id);
        let disk_by_id_key = Self::disk_by_id_key(disk_id);
        let disk_in_instance_key = Self::disk_in_instance_key(instance_id, disk_id);
        let instance_id_str = instance_id.to_string();

        enum Outcome {
            Created(Box<InstanceCreateResult>),
            ProjectMissingOrWrongSilo,
            ImageMissingOrWrongSilo,
            SubnetMissingOrWrongParent,
            SshKeyMissingOrWrongSilo,
            NameTaken,
            IpPoolExhausted,
        }

        let req_for_txn = req.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let project_check_key = project_check_key.clone();
                let image_check_key = image_check_key.clone();
                let subnet_check_key = subnet_check_key.clone();
                let ssh_key_check_keys = ssh_key_check_keys.clone();
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_project_key = in_project_key.clone();
                let nic_by_id_key = nic_by_id_key.clone();
                let nic_in_instance_key = nic_in_instance_key.clone();
                let disk_by_id_key = disk_by_id_key.clone();
                let disk_in_instance_key = disk_in_instance_key.clone();
                let v4_begin = v4_begin.clone();
                let v4_end = v4_end.clone();
                let v6_begin = v6_begin.clone();
                let v6_end = v6_end.clone();
                let id_bytes = instance_id_str.as_bytes().to_vec();
                let req = req_for_txn.clone();
                async move {
                    // Project
                    let project_bytes = match tr.get(&project_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::ProjectMissingOrWrongSilo),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongSilo),
                    };
                    if project.silo_id != silo_id {
                        return Ok(Outcome::ProjectMissingOrWrongSilo);
                    }
                    // Image
                    let image_bytes = match tr.get(&image_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::ImageMissingOrWrongSilo),
                    };
                    let image: Image = match serde_json::from_slice(&image_bytes) {
                        Ok(i) => i,
                        Err(_) => return Ok(Outcome::ImageMissingOrWrongSilo),
                    };
                    if image.silo_id != silo_id {
                        return Ok(Outcome::ImageMissingOrWrongSilo);
                    }
                    // Subnet
                    let subnet_bytes = match tr.get(&subnet_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::SubnetMissingOrWrongParent),
                    };
                    let subnet: Subnet = match serde_json::from_slice(&subnet_bytes) {
                        Ok(s) => s,
                        Err(_) => return Ok(Outcome::SubnetMissingOrWrongParent),
                    };
                    if subnet.silo_id != silo_id || subnet.project_id != project_id {
                        return Ok(Outcome::SubnetMissingOrWrongParent);
                    }
                    // SSH keys
                    for (_key_id, key_check_key) in &ssh_key_check_keys {
                        let key_bytes = match tr.get(key_check_key, false).await? {
                            Some(b) => b,
                            None => return Ok(Outcome::SshKeyMissingOrWrongSilo),
                        };
                        let key: SshKey = match serde_json::from_slice(&key_bytes) {
                            Ok(k) => k,
                            Err(_) => return Ok(Outcome::SshKeyMissingOrWrongSilo),
                        };
                        if key.silo_id != silo_id {
                            return Ok(Outcome::SshKeyMissingOrWrongSilo);
                        }
                    }
                    // Name uniqueness
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }

                    // Read existing IP allocations for this subnet to
                    // feed into the in-memory allocator.
                    let mut allocated_v4: std::collections::HashSet<std::net::Ipv4Addr> =
                        std::collections::HashSet::new();
                    {
                        let opt = RangeOption {
                            begin: KeySelector::first_greater_or_equal(v4_begin),
                            end: KeySelector::first_greater_or_equal(v4_end),
                            ..RangeOption::default()
                        };
                        let kvs = tr.get_range(&opt, 1, false).await?;
                        for kv in kvs.iter() {
                            let suffix = &kv.key()[v4_prefix_len..];
                            if let Ok(s) = std::str::from_utf8(suffix)
                                && let Ok(ip) = s.parse::<std::net::Ipv4Addr>()
                            {
                                allocated_v4.insert(ip);
                            }
                        }
                    }
                    let mut allocated_v6: std::collections::HashSet<std::net::Ipv6Addr> =
                        std::collections::HashSet::new();
                    {
                        let opt = RangeOption {
                            begin: KeySelector::first_greater_or_equal(v6_begin),
                            end: KeySelector::first_greater_or_equal(v6_end),
                            ..RangeOption::default()
                        };
                        let kvs = tr.get_range(&opt, 1, false).await?;
                        for kv in kvs.iter() {
                            let suffix = &kv.key()[v6_prefix_len..];
                            if let Ok(s) = std::str::from_utf8(suffix)
                                && let Ok(ip) = s.parse::<std::net::Ipv6Addr>()
                            {
                                allocated_v6.insert(ip);
                            }
                        }
                    }

                    let primary_ipv4 = match subnet.ipv4_block {
                        Some(cidr) => match crate::types::allocate_ipv4(cidr, &allocated_v4) {
                            Some(ip) => Some(ip),
                            None => return Ok(Outcome::IpPoolExhausted),
                        },
                        None => None,
                    };
                    let primary_ipv6 = match subnet.ipv6_block {
                        Some(cidr) => match crate::types::allocate_ipv6(cidr, &allocated_v6) {
                            Some(ip) => Some(ip),
                            None => return Ok(Outcome::IpPoolExhausted),
                        },
                        None => None,
                    };

                    let now = Utc::now();
                    let mut rng = rand::rng();
                    let nic = Nic {
                        id: nic_id,
                        silo_id,
                        project_id,
                        instance_id,
                        vpc_id: subnet.vpc_id,
                        subnet_id: subnet.id,
                        name: "primary".to_string(),
                        mac: crate::types::generate_mac(&mut rng),
                        primary_ipv4,
                        primary_ipv6,
                        created_at: now,
                    };
                    let instance = Instance {
                        id: instance_id,
                        silo_id,
                        project_id,
                        name: req.name.clone(),
                        description: req.description.unwrap_or_default(),
                        image_id: req.image_id,
                        primary_subnet_id: req.primary_subnet_id,
                        ssh_key_ids: req.ssh_key_ids,
                        cpu: req.cpu,
                        memory_bytes: req.memory_bytes,
                        lifecycle: LifecycleState::Pending,
                        created_at: now,
                        updated_at: now,
                    };
                    let boot_disk = Disk {
                        id: disk_id,
                        silo_id,
                        project_id,
                        instance_id,
                        name: "boot".to_string(),
                        description: format!("Boot disk for instance {}", instance.name),
                        kind: DiskKind::Boot,
                        size_bytes: image.size_bytes,
                        source_image_id: Some(image.id),
                        created_at: now,
                    };
                    let instance_value = match serde_json::to_vec(&instance) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::NameTaken),
                    };
                    let nic_value = match serde_json::to_vec(&nic) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::NameTaken),
                    };
                    let disk_value = match serde_json::to_vec(&boot_disk) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::NameTaken),
                    };
                    tr.set(&by_id_key, &instance_value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_project_key, b"");
                    tr.set(&nic_by_id_key, &nic_value);
                    tr.set(&nic_in_instance_key, b"");
                    tr.set(&disk_by_id_key, &disk_value);
                    tr.set(&disk_in_instance_key, b"");
                    if let Some(ip) = primary_ipv4 {
                        let alloc_key = Self::nic_ip_alloc_v4_key(subnet.id, ip);
                        tr.set(&alloc_key, b"");
                    }
                    if let Some(ip) = primary_ipv6 {
                        let alloc_key = Self::nic_ip_alloc_v6_key(subnet.id, ip);
                        tr.set(&alloc_key, b"");
                    }
                    Ok(Outcome::Created(Box::new(InstanceCreateResult {
                        instance,
                        nics: vec![nic],
                        disks: vec![boot_disk],
                    })))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(both)) => Ok(*both),
            Ok(Outcome::ProjectMissingOrWrongSilo)
            | Ok(Outcome::ImageMissingOrWrongSilo)
            | Ok(Outcome::SubnetMissingOrWrongParent)
            | Ok(Outcome::SshKeyMissingOrWrongSilo) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "instance with name {:?} already exists in project {project_id}",
                req.name
            ))),
            Ok(Outcome::IpPoolExhausted) => Err(StoreError::Backend(format!(
                "subnet {} ip pool exhausted",
                req.primary_subnet_id
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_instance(&self, instance_id: Uuid) -> Result<Instance, StoreError> {
        let key = Self::instance_by_id_key(instance_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize instance: {e}")))
    }

    async fn list_instances_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Instance>, StoreError> {
        let prefix = Self::instance_in_project_prefix(project_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("instance index uuid: {e}")))?;
            let by_id_key = Self::instance_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let instance: Instance = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize instance: {e}")))?;
                out.push(instance);
            }
        }
        Ok(out)
    }

    async fn delete_instance(&self, instance_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = Self::instance_by_id_key(instance_id);
        let nic_prefix = Self::nic_in_instance_prefix(instance_id);
        let (nic_begin, nic_end) = prefix_range(&nic_prefix);
        let nic_prefix_len = nic_prefix.len();
        let disk_prefix = Self::disk_in_instance_prefix(instance_id);
        let (disk_begin, disk_end) = prefix_range(&disk_prefix);
        let disk_prefix_len = disk_prefix.len();

        enum Outcome {
            Deleted,
            Vanished,
            NotDeletable(LifecycleStateKind),
        }
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let nic_begin = nic_begin.clone();
                let nic_end = nic_end.clone();
                let disk_begin = disk_begin.clone();
                let disk_end = disk_end.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::Vanished),
                    };
                    let instance: Instance = match serde_json::from_slice(&bytes) {
                        Ok(i) => i,
                        Err(_) => return Ok(Outcome::Vanished),
                    };
                    if !instance.lifecycle.is_deletable() {
                        return Ok(Outcome::NotDeletable(instance.lifecycle.kind()));
                    }
                    let by_name_key = format!(
                        "instance/by_project/{}/{}",
                        instance.project_id, instance.name
                    )
                    .into_bytes();
                    let in_project_key = format!(
                        "instance/in_project/{}/{}",
                        instance.project_id, instance.id
                    )
                    .into_bytes();

                    // Cascade: discover NIC ids via the in-instance
                    // index, then read each NIC record to free its
                    // IP allocations.
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(nic_begin),
                        end: KeySelector::first_greater_or_equal(nic_end),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    let mut nic_ids: Vec<Uuid> = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[nic_prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix)
                            && let Ok(id) = Uuid::parse_str(s)
                        {
                            nic_ids.push(id);
                        }
                    }
                    drop(kvs);

                    for nic_id in nic_ids {
                        let nic_key = format!("nic/by_id/{nic_id}").into_bytes();
                        let nic_in_instance_key =
                            format!("nic/in_instance/{instance_id}/{nic_id}").into_bytes();
                        let nic_bytes = match tr.get(&nic_key, false).await? {
                            Some(b) => b,
                            None => {
                                // Membership index is the source of
                                // truth for what to clean up; if the
                                // record is gone, just clear the
                                // index entry and move on.
                                tr.clear(&nic_in_instance_key);
                                continue;
                            }
                        };
                        let nic: Nic = match serde_json::from_slice(&nic_bytes) {
                            Ok(n) => n,
                            Err(_) => {
                                tr.clear(&nic_key);
                                tr.clear(&nic_in_instance_key);
                                continue;
                            }
                        };
                        if let Some(ip) = nic.primary_ipv4 {
                            tr.clear(&Self::nic_ip_alloc_v4_key(nic.subnet_id, ip));
                        }
                        if let Some(ip) = nic.primary_ipv6 {
                            tr.clear(&Self::nic_ip_alloc_v6_key(nic.subnet_id, ip));
                        }
                        tr.clear(&nic_key);
                        tr.clear(&nic_in_instance_key);
                    }

                    // Cascade disks: discover, then clear each
                    // disk record + its membership entry.
                    let disk_opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(disk_begin),
                        end: KeySelector::first_greater_or_equal(disk_end),
                        ..RangeOption::default()
                    };
                    let disk_kvs = tr.get_range(&disk_opt, 1, false).await?;
                    let mut disk_ids: Vec<Uuid> = Vec::new();
                    for kv in disk_kvs.iter() {
                        let suffix = &kv.key()[disk_prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix)
                            && let Ok(id) = Uuid::parse_str(s)
                        {
                            disk_ids.push(id);
                        }
                    }
                    drop(disk_kvs);
                    for disk_id in disk_ids {
                        let dk = format!("disk/by_id/{disk_id}").into_bytes();
                        let dki = format!("disk/in_instance/{instance_id}/{disk_id}").into_bytes();
                        tr.clear(&dk);
                        tr.clear(&dki);
                    }

                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_project_key);
                    Ok(Outcome::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Deleted) => Ok(()),
            Ok(Outcome::Vanished) => Err(StoreError::NotFound),
            Ok(Outcome::NotDeletable(kind)) => Err(StoreError::Conflict(format!(
                "instance {instance_id} is not deletable in state {kind:?}; stop it first"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn transition_instance_lifecycle(
        &self,
        instance_id: Uuid,
        expected_from: &[LifecycleStateKind],
        to: LifecycleState,
    ) -> Result<Instance, StoreError> {
        let by_id_key = Self::instance_by_id_key(instance_id);
        let expected_from_owned = expected_from.to_vec();

        enum Outcome {
            Updated(Box<Instance>),
            Vanished,
            WrongState(LifecycleStateKind),
        }
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let expected_from = expected_from_owned.clone();
                let to = to.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::Vanished),
                    };
                    let mut instance: Instance = match serde_json::from_slice(&bytes) {
                        Ok(i) => i,
                        Err(_) => return Ok(Outcome::Vanished),
                    };
                    let current = instance.lifecycle.kind();
                    if !expected_from.contains(&current) {
                        return Ok(Outcome::WrongState(current));
                    }
                    instance.lifecycle = to;
                    instance.updated_at = Utc::now();
                    let value = match serde_json::to_vec(&instance) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::Vanished),
                    };
                    tr.set(&by_id_key, &value);
                    Ok(Outcome::Updated(Box::new(instance)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated(i)) => Ok(*i),
            Ok(Outcome::Vanished) => Err(StoreError::NotFound),
            Ok(Outcome::WrongState(kind)) => Err(StoreError::Conflict(format!(
                "instance {instance_id} is in {kind:?}; expected one of {expected_from:?}"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_nic(&self, nic_id: Uuid) -> Result<Nic, StoreError> {
        let key = Self::nic_by_id_key(nic_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize nic: {e}")))
    }

    async fn get_disk(&self, disk_id: Uuid) -> Result<Disk, StoreError> {
        let key = Self::disk_by_id_key(disk_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize disk: {e}")))
    }

    async fn list_disks_for_instance(&self, instance_id: Uuid) -> Result<Vec<Disk>, StoreError> {
        let prefix = Self::disk_in_instance_prefix(instance_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("disk index uuid: {e}")))?;
            let by_id_key = Self::disk_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let disk: Disk = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize disk: {e}")))?;
                out.push(disk);
            }
        }
        Ok(out)
    }

    async fn list_nics_for_instance(&self, instance_id: Uuid) -> Result<Vec<Nic>, StoreError> {
        let prefix = Self::nic_in_instance_prefix(instance_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
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
                    let mut ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix) {
                            ids.push(s.to_string());
                        }
                    }
                    Ok(ids)
                }
            })
            .await;
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("nic index uuid: {e}")))?;
            let by_id_key = Self::nic_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let nic: Nic = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize nic: {e}")))?;
                out.push(nic);
            }
        }
        Ok(out)
    }

    async fn enqueue_job(&self, req: NewJob) -> Result<ProvisioningJob, StoreError> {
        let counter_key = Self::job_seq_counter_key().to_vec();
        let job_id = Uuid::new_v4();
        let by_id_key = Self::job_by_id_key(job_id);
        let id_str = job_id.to_string();

        let outcome: Result<ProvisioningJob, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let counter_key = counter_key.clone();
                let by_id_key = by_id_key.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                let kind = req.kind.clone();
                async move {
                    // Read counter (or default to 0).
                    let current_seq = match tr.get(&counter_key, false).await? {
                        Some(bytes) => parse_seq(&bytes).unwrap_or(0),
                        None => 0,
                    };
                    let next_seq = current_seq.saturating_add(1);
                    let pending_key = Self::job_pending_key(current_seq);
                    let job = ProvisioningJob {
                        id: job_id,
                        kind,
                        status: JobStatus::Pending,
                        seq: current_seq,
                        created_at: Utc::now(),
                        claimed_at: None,
                        claimed_by: None,
                        completed_at: None,
                    };
                    let value = serde_json::to_vec(&job).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize job: {e}").into())
                    })?;
                    tr.set(&counter_key, &next_seq.to_be_bytes());
                    tr.set(&by_id_key, &value);
                    tr.set(&pending_key, &id_bytes);
                    Ok(job)
                }
            })
            .await;
        outcome.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))
    }

    async fn claim_next_job(&self, claimed_by: &str) -> Result<ProvisioningJob, StoreError> {
        let prefix = Self::job_pending_prefix().to_vec();
        let (begin, end) = prefix_range(&prefix);
        let claimed_by = claimed_by.to_string();

        enum Outcome {
            Claimed(Box<ProvisioningJob>),
            Empty,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                let claimed_by = claimed_by.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        limit: Some(1),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    // Pick the (single) lowest-seq pending entry.
                    let (pending_key, job_id_bytes) = match kvs.iter().next() {
                        Some(kv) => (kv.key().to_vec(), kv.value().to_vec()),
                        None => return Ok(Outcome::Empty),
                    };
                    drop(kvs);
                    let id_str = std::str::from_utf8(&job_id_bytes).map_err(|e| {
                        FdbBindingError::CustomError(
                            format!("pending index value not utf8: {e}").into(),
                        )
                    })?;
                    let job_id = Uuid::parse_str(id_str).map_err(|e| {
                        FdbBindingError::CustomError(
                            format!("pending index value not uuid: {e}").into(),
                        )
                    })?;
                    let by_id_key = format!("job/by_id/{job_id}").into_bytes();
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        // Pending index points at a vanished record;
                        // skip the entry by clearing it and treating
                        // the queue as empty for this attempt.
                        None => {
                            tr.clear(&pending_key);
                            return Ok(Outcome::Empty);
                        }
                    };
                    let mut job: ProvisioningJob = serde_json::from_slice(&bytes).map_err(|e| {
                        FdbBindingError::CustomError(format!("deserialize job: {e}").into())
                    })?;
                    job.status = JobStatus::InProgress;
                    job.claimed_at = Some(Utc::now());
                    job.claimed_by = Some(claimed_by);
                    let value = serde_json::to_vec(&job).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize job: {e}").into())
                    })?;
                    tr.set(&by_id_key, &value);
                    tr.clear(&pending_key);
                    Ok(Outcome::Claimed(Box::new(job)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Claimed(job)) => Ok(*job),
            Ok(Outcome::Empty) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn complete_job(
        &self,
        job_id: Uuid,
        outcome: JobOutcome,
    ) -> Result<ProvisioningJob, StoreError> {
        let by_id_key = Self::job_by_id_key(job_id);

        enum Out {
            Completed(Box<ProvisioningJob>),
            Vanished,
            AlreadyTerminal(JobStatusKind),
        }

        let result: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let outcome = outcome.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Out::Vanished),
                    };
                    let mut job: ProvisioningJob = match serde_json::from_slice(&bytes) {
                        Ok(j) => j,
                        Err(_) => return Ok(Out::Vanished),
                    };
                    let kind = job.status.kind();
                    if matches!(kind, JobStatusKind::Completed | JobStatusKind::Failed) {
                        return Ok(Out::AlreadyTerminal(kind));
                    }
                    job.status = match outcome {
                        JobOutcome::Completed => JobStatus::Completed,
                        JobOutcome::Failed { reason } => JobStatus::Failed { reason },
                    };
                    job.completed_at = Some(Utc::now());
                    let value = match serde_json::to_vec(&job) {
                        Ok(v) => v,
                        Err(_) => return Ok(Out::Vanished),
                    };
                    tr.set(&by_id_key, &value);
                    Ok(Out::Completed(Box::new(job)))
                }
            })
            .await;

        match result {
            Ok(Out::Completed(job)) => Ok(*job),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::AlreadyTerminal(k)) => Err(StoreError::Conflict(format!(
                "job {job_id} is already terminal ({k:?})"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_job(&self, job_id: Uuid) -> Result<ProvisioningJob, StoreError> {
        let key = Self::job_by_id_key(job_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize job: {e}")))
    }

    async fn list_recent_jobs(&self, limit: usize) -> Result<Vec<ProvisioningJob>, StoreError> {
        // Recent = scan all by_id and sort. Phase 0 has no
        // creation-time index because the queue's normal hot path
        // is `claim_next_job`; the operator-visible "recent jobs"
        // surface is rare and small. A future slice can add a
        // dedicated by_created_at index if list_recent_jobs becomes
        // a hot path.
        let prefix = b"job/by_id/".to_vec();
        let (begin, end) = prefix_range(&prefix);

        let raws: Result<Vec<Vec<u8>>, FdbBindingError> = self
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
        let raws = raws.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut jobs: Vec<ProvisioningJob> = raws
            .into_iter()
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect();
        jobs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.seq.cmp(&a.seq))
        });
        jobs.truncate(limit);
        Ok(jobs)
    }
}

/// Parse an 8-byte big-endian counter value.
fn parse_seq(bytes: &[u8]) -> Option<u64> {
    if bytes.len() != 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(bytes);
    Some(u64::from_be_bytes(buf))
}

/// Length of the `subnet/in_vpc/<vpc_id>/` prefix bytes for a given
/// VPC id. Used inside the transaction to slice peer-id suffixes
/// without recomputing the prefix on every iteration.
fn subnet_prefix_len(vpc_id: Uuid) -> usize {
    format!("subnet/in_vpc/{vpc_id}/").len()
}

impl FdbStore {
    /// Read the value for a single key, returning `None` if absent.
    async fn read_bytes(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let key = key.to_vec();
        let result: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;
        result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))
    }
}
