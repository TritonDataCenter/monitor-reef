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
    ApiKey, IdpConfig, Image, NewImage, NewProject, NewQuota, NewSilo, NewSshKey, NewSubnet,
    NewVpc, Project, Quota, Silo, SshKey, Store, StoreError, Subnet, SystemKey, User, VPC_VNI_MAX,
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
