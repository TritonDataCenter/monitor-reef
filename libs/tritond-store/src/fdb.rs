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
//! tenant/by_id/<uuid>               -> JSON-encoded Tenant
//! tenant/by_silo/<silo>/<name>      -> uuid hyphenated bytes
//! tenant/in_silo/<silo>/<tenant>    -> empty (membership index)
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
//! floating_ip/by_id/<uuid>          -> JSON-encoded FloatingIp
//! floating_ip/by_project/<p>/<name> -> uuid hyphenated bytes
//! floating_ip/in_project/<p>/<fip>  -> empty (membership index)
//! floating_ip/alloc/v4/<addr>       -> empty (pool allocation, v4)
//! floating_ip/alloc/v6/<addr>       -> empty (pool allocation, v6)
//! job/by_id/<uuid>                  -> JSON-encoded ProvisioningJob
//! job/pending/<seq-be-u64>          -> uuid hyphenated bytes (FIFO queue)
//! job/seq/counter                   -> next seq, big-endian u64
//! cn/by_uuid/<server_uuid>          -> JSON-encoded Cn
//! cn/by_claim/<normalized_code>     -> server_uuid hyphenated bytes
//! cn/by_poll/<poll_token>           -> server_uuid hyphenated bytes
//! cn/by_state/<state>/<server_uuid> -> empty (membership index)
//! auto_approve/window               -> JSON-encoded AutoApproveWindow (singleton)
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
    AddressFamily, ApiKey, AutoApproveWindow, CLAIM_CODE_TTL, Cn, CnState, Disk, DiskKind,
    FLOATING_IP_V4_POOL, FLOATING_IP_V6_POOL, FloatingIp, FloatingIpAttachment, IdpConfig, Image,
    Instance, InstanceCreateResult, JobOutcome, JobStatus, JobStatusKind, LifecycleState,
    LifecycleStateKind, NewFloatingIp, NewImage, NewInstance, NewJob, NewProject, NewQuota,
    NewSilo, NewSshKey, NewSubnet, NewTenant, NewVpc, Nic, Project, ProvisioningJob, Quota, Silo,
    SshKey, Store, StoreError, Subnet, SystemKey, Tenant, User, VPC_VNI_MAX,
    VPC_VNI_RESERVED_CEILING, Vpc, generate_claim_code, generate_poll_token,
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

    fn tenant_by_id_key(id: Uuid) -> Vec<u8> {
        format!("tenant/by_id/{id}").into_bytes()
    }

    fn tenant_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
        format!("tenant/by_silo/{silo_id}/{name}").into_bytes()
    }

    fn tenant_in_silo_key(silo_id: Uuid, tenant_id: Uuid) -> Vec<u8> {
        format!("tenant/in_silo/{silo_id}/{tenant_id}").into_bytes()
    }

    fn tenant_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
        format!("tenant/in_silo/{silo_id}/").into_bytes()
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

    fn floating_ip_by_id_key(id: Uuid) -> Vec<u8> {
        format!("floating_ip/by_id/{id}").into_bytes()
    }

    fn floating_ip_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
        format!("floating_ip/by_project/{project_id}/{name}").into_bytes()
    }

    fn floating_ip_in_project_key(project_id: Uuid, fip_id: Uuid) -> Vec<u8> {
        format!("floating_ip/in_project/{project_id}/{fip_id}").into_bytes()
    }

    fn floating_ip_in_project_prefix(project_id: Uuid) -> Vec<u8> {
        format!("floating_ip/in_project/{project_id}/").into_bytes()
    }

    fn floating_ip_alloc_v4_key(ip: std::net::Ipv4Addr) -> Vec<u8> {
        format!("floating_ip/alloc/v4/{ip}").into_bytes()
    }

    fn floating_ip_alloc_v6_key(ip: std::net::Ipv6Addr) -> Vec<u8> {
        format!("floating_ip/alloc/v6/{ip}").into_bytes()
    }

    fn floating_ip_alloc_v4_prefix() -> &'static [u8] {
        b"floating_ip/alloc/v4/"
    }

    fn floating_ip_alloc_v6_prefix() -> &'static [u8] {
        b"floating_ip/alloc/v6/"
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

    fn cn_by_uuid_key(server_uuid: Uuid) -> Vec<u8> {
        format!("cn/by_uuid/{server_uuid}").into_bytes()
    }

    fn cn_by_claim_key(normalized_code: &str) -> Vec<u8> {
        format!("cn/by_claim/{normalized_code}").into_bytes()
    }

    fn cn_by_poll_key(poll_token: &str) -> Vec<u8> {
        format!("cn/by_poll/{poll_token}").into_bytes()
    }

    fn cn_by_state_key(state: CnState, server_uuid: Uuid) -> Vec<u8> {
        format!("cn/by_state/{}/{server_uuid}", cn_state_tag(state)).into_bytes()
    }

    fn cn_by_state_prefix(state: CnState) -> Vec<u8> {
        format!("cn/by_state/{}/", cn_state_tag(state)).into_bytes()
    }

    fn auto_approve_window_key() -> &'static [u8] {
        b"auto_approve/window"
    }
}

/// Map [`CnState`] to its serde wire-format tag (matching the
/// `#[serde(rename_all = "snake_case")]` on the enum).
fn cn_state_tag(state: CnState) -> &'static str {
    match state {
        CnState::Pending => "pending",
        CnState::Approved => "approved",
        CnState::Disabled => "disabled",
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

/// Job targeting matrix: unrouted jobs (target=None) are claimable
/// by anyone; routed jobs (target=Some(X)) are claimable only by
/// the bound claimer for X. Mirrors the in-memory store's helper.
fn targeting_matches(job_target: Option<Uuid>, claimer_cn: Option<Uuid>) -> bool {
    match (job_target, claimer_cn) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(t), Some(c)) => t == c,
    }
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
        // Atomic two-record write: create the silo and its default
        // tenant in a single FDB transaction so a federated login
        // can never race with silo creation and observe a
        // tenant-less silo.
        let silo_id = Uuid::new_v4();
        let now = Utc::now();
        let tenant = Tenant {
            id: Uuid::new_v4(),
            silo_id,
            name: "default".to_string(),
            description: format!("Default tenant for silo {}", req.name),
            created_at: now,
        };
        let silo = Silo {
            id: silo_id,
            name: req.name,
            description: req.description.unwrap_or_default(),
            default_tenant_id: tenant.id,
            created_at: now,
        };
        let silo_value = serde_json::to_vec(&silo)
            .map_err(|e| StoreError::Backend(format!("serialize silo: {e}")))?;
        let tenant_value = serde_json::to_vec(&tenant)
            .map_err(|e| StoreError::Backend(format!("serialize tenant: {e}")))?;
        let silo_by_id_key = Self::silo_by_id_key(silo.id);
        let silo_by_name_key = Self::silo_by_name_key(&silo.name);
        let tenant_by_id_key = Self::tenant_by_id_key(tenant.id);
        let tenant_by_name_key = Self::tenant_by_silo_name_key(silo_id, &tenant.name);
        let tenant_in_silo_key = Self::tenant_in_silo_key(silo_id, tenant.id);
        let silo_id_str = silo.id.to_string();
        let tenant_id_str = tenant.id.to_string();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let silo_by_id_key = silo_by_id_key.clone();
                let silo_by_name_key = silo_by_name_key.clone();
                let tenant_by_id_key = tenant_by_id_key.clone();
                let tenant_by_name_key = tenant_by_name_key.clone();
                let tenant_in_silo_key = tenant_in_silo_key.clone();
                let silo_value = silo_value.clone();
                let tenant_value = tenant_value.clone();
                let silo_id_bytes = silo_id_str.as_bytes().to_vec();
                let tenant_id_bytes = tenant_id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&silo_by_name_key, false).await?.is_some() {
                        return Ok(CreateOutcome::NameTaken);
                    }
                    tr.set(&silo_by_id_key, &silo_value);
                    tr.set(&silo_by_name_key, &silo_id_bytes);
                    tr.set(&tenant_by_id_key, &tenant_value);
                    tr.set(&tenant_by_name_key, &tenant_id_bytes);
                    tr.set(&tenant_in_silo_key, b"");
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
        // Federation index is keyed by (silo_id, issuer, subject) —
        // the IdP belongs to the silo, not the tenant. Resolve the
        // user's tenant outside the transaction so we can derive
        // the owning silo. This is a defensive read; a missing
        // tenant for a federated user is a programming error.
        let federation_key = match (user.tenant_id, user.federation.as_ref()) {
            (Some(tenant_id), Some(fed)) => {
                let tenant = self.get_tenant(tenant_id).await?;
                Some(Self::user_federation_key(
                    tenant.silo_id,
                    &fed.issuer,
                    &fed.subject,
                ))
            }
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

    async fn create_tenant(&self, silo_id: Uuid, req: NewTenant) -> Result<Tenant, StoreError> {
        let tenant = Tenant {
            id: Uuid::new_v4(),
            silo_id,
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&tenant)
            .map_err(|e| StoreError::Backend(format!("serialize tenant: {e}")))?;
        let by_id_key = Self::tenant_by_id_key(tenant.id);
        let by_name_key = Self::tenant_by_silo_name_key(silo_id, &tenant.name);
        let in_silo_key = Self::tenant_in_silo_key(silo_id, tenant.id);
        let silo_check_key = Self::silo_by_id_key(silo_id);
        let id_str = tenant.id.to_string();
        let name_str = tenant.name.clone();

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
            Ok(Outcome::Created) => Ok(tenant),
            Ok(Outcome::SiloMissing) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "tenant with name {name_str:?} already exists in silo {silo_id}"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_tenant(&self, tenant_id: Uuid) -> Result<Tenant, StoreError> {
        let key = Self::tenant_by_id_key(tenant_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize tenant: {e}")))
    }

    async fn list_tenants_in_silo(&self, silo_id: Uuid) -> Result<Vec<Tenant>, StoreError> {
        // Confirm the silo exists first so callers can distinguish
        // "silo missing" (NotFound) from "silo present but empty"
        // (empty Vec).
        let silo_check_key = Self::silo_by_id_key(silo_id);
        if self.read_bytes(&silo_check_key).await?.is_none() {
            return Err(StoreError::NotFound);
        }

        let prefix = Self::tenant_in_silo_prefix(silo_id);
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
                .map_err(|e| StoreError::Backend(format!("tenant index uuid: {e}")))?;
            let by_id_key = Self::tenant_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let tenant: Tenant = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize tenant: {e}")))?;
                out.push(tenant);
            }
        }
        Ok(out)
    }

    async fn delete_tenant(&self, tenant_id: Uuid) -> Result<(), StoreError> {
        // Read the row outside the transaction so we know the
        // silo_id + name to clear from the indices. Concurrent
        // delete shows up as DelOut::Vanished below.
        let by_id_key = Self::tenant_by_id_key(tenant_id);
        let bytes = match self.read_bytes(&by_id_key).await? {
            Some(b) => b,
            None => return Err(StoreError::NotFound),
        };
        let tenant: Tenant = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize tenant: {e}")))?;
        let by_name_key = Self::tenant_by_silo_name_key(tenant.silo_id, &tenant.name);
        let in_silo_key = Self::tenant_in_silo_key(tenant.silo_id, tenant.id);

        // TODO(slice E-3): reject deletion when child projects exist
        let outcome: Result<DeleteOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_silo_key = in_silo_key.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DeleteOutcome::NotFound);
                    }
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_silo_key);
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
        // None → derive from sha256 (the new default), which makes
        // tritond's image identity content-addressed and lets the
        // per-CN agent share one ZFS dataset across silos when
        // operators register the same content under different
        // names. Some(...) → operator pinned, used for cross-cluster
        // mirror cases.
        let id = req
            .id
            .unwrap_or_else(|| crate::derive_image_id(silo_id, &req.sha256));
        let image = Image {
            id,
            silo_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            os: req.os,
            version: req.version,
            size_bytes: req.size_bytes,
            sha256: req.sha256,
            source_url: req.source_url,
            compatibility: req.compatibility,
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
            IdTaken,
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
                    // When `req.id` was set, the by_id key may
                    // already exist for an unrelated image — we
                    // check inside the transaction so a concurrent
                    // pin-id create can't race past us.
                    if tr.get(&by_id_key, false).await?.is_some() {
                        return Ok(Outcome::IdTaken);
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
            Ok(Outcome::IdTaken) => Err(StoreError::Conflict(format!(
                "image with id {} already exists",
                image.id,
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

        // Per-extra-NIC precomputed keys + ids. Cloned into the
        // closure each iteration; the Vec itself is captured by
        // move + cloned per-attempt.
        #[derive(Clone)]
        struct ExtraNicTxnPlan {
            spec_subnet_id: Uuid,
            name: String,
            nic_id: Uuid,
            subnet_check_key: Vec<u8>,
            v4_begin: Vec<u8>,
            v4_end: Vec<u8>,
            v6_begin: Vec<u8>,
            v6_end: Vec<u8>,
            v4_prefix_len: usize,
            v6_prefix_len: usize,
            nic_by_id_key: Vec<u8>,
            nic_in_instance_key: Vec<u8>,
        }
        let extra_plans: Vec<ExtraNicTxnPlan> = req
            .extra_nics
            .iter()
            .map(|spec| {
                let v4_prefix = Self::nic_ip_alloc_v4_prefix(spec.subnet_id);
                let v6_prefix = Self::nic_ip_alloc_v6_prefix(spec.subnet_id);
                let (v4_begin, v4_end) = prefix_range(&v4_prefix);
                let (v6_begin, v6_end) = prefix_range(&v6_prefix);
                let nid = Uuid::new_v4();
                ExtraNicTxnPlan {
                    spec_subnet_id: spec.subnet_id,
                    name: spec.name.clone(),
                    nic_id: nid,
                    subnet_check_key: Self::subnet_by_id_key(spec.subnet_id),
                    v4_prefix_len: v4_prefix.len(),
                    v6_prefix_len: v6_prefix.len(),
                    v4_begin,
                    v4_end,
                    v6_begin,
                    v6_end,
                    nic_by_id_key: Self::nic_by_id_key(nid),
                    nic_in_instance_key: Self::nic_in_instance_key(instance_id, nid),
                }
            })
            .collect();

        enum Outcome {
            Created(Box<InstanceCreateResult>),
            ProjectMissingOrWrongSilo,
            ImageMissingOrWrongSilo,
            SubnetMissingOrWrongParent,
            SshKeyMissingOrWrongSilo,
            NameTaken,
            IpPoolExhausted,
            DuplicateNicName(String),
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
                let extra_plans = extra_plans.clone();
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
                    // ThreadRng is !Send so we can't hold it across
                    // the await points in the extra-NIC loop below.
                    // Spin a fresh one per NIC right before the MAC
                    // generation (which is synchronous).
                    let nic = Nic {
                        id: nic_id,
                        silo_id,
                        project_id,
                        instance_id,
                        vpc_id: subnet.vpc_id,
                        subnet_id: subnet.id,
                        name: "primary".to_string(),
                        mac: {
                            let mut rng = rand::rng();
                            crate::types::generate_mac(&mut rng)
                        },
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

                    // Extra NICs. Same pattern as the primary,
                    // inside the same transaction so a partial
                    // failure rolls back cleanly. Allocations
                    // accumulated within this txn are added to
                    // local v4/v6 sets so two extras drawing from
                    // the same subnet don't collide on the same IP.
                    let mut nic_records: Vec<Nic> = vec![nic];
                    for plan in &extra_plans {
                        // Spec name uniqueness within this instance.
                        if nic_records.iter().any(|n| n.name == plan.name) {
                            return Ok(Outcome::DuplicateNicName(plan.name.clone()));
                        }
                        // Resolve subnet.
                        let extra_subnet_bytes = match tr.get(&plan.subnet_check_key, false).await?
                        {
                            Some(b) => b,
                            None => return Ok(Outcome::SubnetMissingOrWrongParent),
                        };
                        let extra_subnet: Subnet = match serde_json::from_slice(&extra_subnet_bytes)
                        {
                            Ok(s) => s,
                            Err(_) => return Ok(Outcome::SubnetMissingOrWrongParent),
                        };
                        if extra_subnet.silo_id != silo_id || extra_subnet.project_id != project_id
                        {
                            return Ok(Outcome::SubnetMissingOrWrongParent);
                        }
                        // Read existing v4 + v6 allocations for
                        // this extra subnet.
                        let mut allocated_v4_extra: std::collections::HashSet<std::net::Ipv4Addr> =
                            std::collections::HashSet::new();
                        {
                            let opt = RangeOption {
                                begin: KeySelector::first_greater_or_equal(plan.v4_begin.clone()),
                                end: KeySelector::first_greater_or_equal(plan.v4_end.clone()),
                                ..RangeOption::default()
                            };
                            let kvs = tr.get_range(&opt, 1, false).await?;
                            for kv in kvs.iter() {
                                let suffix = &kv.key()[plan.v4_prefix_len..];
                                if let Ok(s) = std::str::from_utf8(suffix)
                                    && let Ok(ip) = s.parse::<std::net::Ipv4Addr>()
                                {
                                    allocated_v4_extra.insert(ip);
                                }
                            }
                        }
                        let mut allocated_v6_extra: std::collections::HashSet<std::net::Ipv6Addr> =
                            std::collections::HashSet::new();
                        {
                            let opt = RangeOption {
                                begin: KeySelector::first_greater_or_equal(plan.v6_begin.clone()),
                                end: KeySelector::first_greater_or_equal(plan.v6_end.clone()),
                                ..RangeOption::default()
                            };
                            let kvs = tr.get_range(&opt, 1, false).await?;
                            for kv in kvs.iter() {
                                let suffix = &kv.key()[plan.v6_prefix_len..];
                                if let Ok(s) = std::str::from_utf8(suffix)
                                    && let Ok(ip) = s.parse::<std::net::Ipv6Addr>()
                                {
                                    allocated_v6_extra.insert(ip);
                                }
                            }
                        }
                        // If the same subnet appears more than
                        // once across extras (or matches the
                        // primary), prior allocations within
                        // this txn must also be excluded.
                        for n in &nic_records {
                            if n.subnet_id == plan.spec_subnet_id {
                                if let Some(ip) = n.primary_ipv4 {
                                    allocated_v4_extra.insert(ip);
                                }
                                if let Some(ip) = n.primary_ipv6 {
                                    allocated_v6_extra.insert(ip);
                                }
                            }
                        }
                        let extra_v4 = match extra_subnet.ipv4_block {
                            Some(cidr) => {
                                match crate::types::allocate_ipv4(cidr, &allocated_v4_extra) {
                                    Some(ip) => Some(ip),
                                    None => return Ok(Outcome::IpPoolExhausted),
                                }
                            }
                            None => None,
                        };
                        let extra_v6 = match extra_subnet.ipv6_block {
                            Some(cidr) => {
                                match crate::types::allocate_ipv6(cidr, &allocated_v6_extra) {
                                    Some(ip) => Some(ip),
                                    None => return Ok(Outcome::IpPoolExhausted),
                                }
                            }
                            None => None,
                        };
                        let extra_nic = Nic {
                            id: plan.nic_id,
                            silo_id,
                            project_id,
                            instance_id,
                            vpc_id: extra_subnet.vpc_id,
                            subnet_id: extra_subnet.id,
                            name: plan.name.clone(),
                            mac: {
                                let mut rng = rand::rng();
                                crate::types::generate_mac(&mut rng)
                            },
                            primary_ipv4: extra_v4,
                            primary_ipv6: extra_v6,
                            created_at: now,
                        };
                        let extra_value = match serde_json::to_vec(&extra_nic) {
                            Ok(v) => v,
                            Err(_) => return Ok(Outcome::NameTaken),
                        };
                        tr.set(&plan.nic_by_id_key, &extra_value);
                        tr.set(&plan.nic_in_instance_key, b"");
                        if let Some(ip) = extra_v4 {
                            let alloc_key = Self::nic_ip_alloc_v4_key(extra_subnet.id, ip);
                            tr.set(&alloc_key, b"");
                        }
                        if let Some(ip) = extra_v6 {
                            let alloc_key = Self::nic_ip_alloc_v6_key(extra_subnet.id, ip);
                            tr.set(&alloc_key, b"");
                        }
                        nic_records.push(extra_nic);
                    }

                    Ok(Outcome::Created(Box::new(InstanceCreateResult {
                        instance,
                        nics: nic_records,
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
            Ok(Outcome::DuplicateNicName(name)) => Err(StoreError::Conflict(format!(
                "duplicate NIC name {name:?} on instance",
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

    async fn delete_instance(&self, instance_id: Uuid, force: bool) -> Result<(), StoreError> {
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
                    if !force && !instance.lifecycle.is_deletable() {
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

                    // Auto-detach (do NOT release) any FloatingIps
                    // attached to this instance. The IP stays in the
                    // project's pool, ready to re-attach. We discover
                    // the candidates by scanning the project's
                    // floating-ip membership index and matching on
                    // attached_to.instance_id.
                    let fip_prefix =
                        format!("floating_ip/in_project/{}/", instance.project_id).into_bytes();
                    let (fip_begin, fip_end) = prefix_range(&fip_prefix);
                    let fip_prefix_len = fip_prefix.len();
                    let fip_opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(fip_begin),
                        end: KeySelector::first_greater_or_equal(fip_end),
                        ..RangeOption::default()
                    };
                    let fip_kvs = tr.get_range(&fip_opt, 1, false).await?;
                    let mut fip_ids: Vec<Uuid> = Vec::new();
                    for kv in fip_kvs.iter() {
                        let suffix = &kv.key()[fip_prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix)
                            && let Ok(id) = Uuid::parse_str(s)
                        {
                            fip_ids.push(id);
                        }
                    }
                    drop(fip_kvs);
                    let now = Utc::now();
                    for fip_id in fip_ids {
                        let fk = format!("floating_ip/by_id/{fip_id}").into_bytes();
                        let Some(fb) = tr.get(&fk, false).await? else {
                            continue;
                        };
                        let Ok(mut fip) = serde_json::from_slice::<FloatingIp>(&fb) else {
                            continue;
                        };
                        let attached_here = fip
                            .attached_to
                            .as_ref()
                            .map(|a| a.instance_id == instance_id)
                            .unwrap_or(false);
                        if !attached_here {
                            continue;
                        }
                        fip.attached_to = None;
                        fip.updated_at = now;
                        if let Ok(value) = serde_json::to_vec(&fip) {
                            tr.set(&fk, &value);
                        }
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

    async fn create_floating_ip(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewFloatingIp,
    ) -> Result<FloatingIp, StoreError> {
        let project_check_key = Self::project_by_id_key(project_id);
        let by_name_key = Self::floating_ip_by_project_name_key(project_id, &req.name);
        let alloc_v4_prefix = Self::floating_ip_alloc_v4_prefix().to_vec();
        let alloc_v6_prefix = Self::floating_ip_alloc_v6_prefix().to_vec();
        let (v4_begin, v4_end) = prefix_range(&alloc_v4_prefix);
        let (v6_begin, v6_end) = prefix_range(&alloc_v6_prefix);
        let v4_prefix_len = alloc_v4_prefix.len();
        let v6_prefix_len = alloc_v6_prefix.len();

        let fip_id = Uuid::new_v4();
        let by_id_key = Self::floating_ip_by_id_key(fip_id);
        let in_project_key = Self::floating_ip_in_project_key(project_id, fip_id);
        let id_str = fip_id.to_string();

        enum Outcome {
            Created(Box<FloatingIp>),
            ProjectMissingOrWrongSilo,
            NameTaken,
            PoolExhausted,
        }

        let req_for_txn = req.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let project_check_key = project_check_key.clone();
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_project_key = in_project_key.clone();
                let v4_begin = v4_begin.clone();
                let v4_end = v4_end.clone();
                let v6_begin = v6_begin.clone();
                let v6_end = v6_end.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                let req = req_for_txn.clone();
                async move {
                    // Project + same-silo check.
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
                    // Name uniqueness.
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }
                    // Allocate from the appropriate pool.
                    let address: std::net::IpAddr = match req.family {
                        AddressFamily::V4 => {
                            let opt = RangeOption {
                                begin: KeySelector::first_greater_or_equal(v4_begin),
                                end: KeySelector::first_greater_or_equal(v4_end),
                                ..RangeOption::default()
                            };
                            let kvs = tr.get_range(&opt, 1, false).await?;
                            let mut allocated: std::collections::HashSet<std::net::Ipv4Addr> =
                                std::collections::HashSet::new();
                            for kv in kvs.iter() {
                                let suffix = &kv.key()[v4_prefix_len..];
                                if let Ok(s) = std::str::from_utf8(suffix)
                                    && let Ok(ip) = s.parse::<std::net::Ipv4Addr>()
                                {
                                    allocated.insert(ip);
                                }
                            }
                            drop(kvs);
                            match crate::types::allocate_ipv4(FLOATING_IP_V4_POOL, &allocated) {
                                Some(ip) => ip.into(),
                                None => return Ok(Outcome::PoolExhausted),
                            }
                        }
                        AddressFamily::V6 => {
                            let opt = RangeOption {
                                begin: KeySelector::first_greater_or_equal(v6_begin),
                                end: KeySelector::first_greater_or_equal(v6_end),
                                ..RangeOption::default()
                            };
                            let kvs = tr.get_range(&opt, 1, false).await?;
                            let mut allocated: std::collections::HashSet<std::net::Ipv6Addr> =
                                std::collections::HashSet::new();
                            for kv in kvs.iter() {
                                let suffix = &kv.key()[v6_prefix_len..];
                                if let Ok(s) = std::str::from_utf8(suffix)
                                    && let Ok(ip) = s.parse::<std::net::Ipv6Addr>()
                                {
                                    allocated.insert(ip);
                                }
                            }
                            drop(kvs);
                            match crate::types::allocate_ipv6(FLOATING_IP_V6_POOL, &allocated) {
                                Some(ip) => ip.into(),
                                None => return Ok(Outcome::PoolExhausted),
                            }
                        }
                    };
                    let now = Utc::now();
                    let fip = FloatingIp {
                        id: fip_id,
                        silo_id,
                        project_id,
                        name: req.name.clone(),
                        description: req.description.unwrap_or_default(),
                        address,
                        attached_to: None,
                        created_at: now,
                        updated_at: now,
                    };
                    let value = match serde_json::to_vec(&fip) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::PoolExhausted), // surrogate; treated as backend
                    };
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_project_key, b"");
                    match address {
                        std::net::IpAddr::V4(v4) => {
                            tr.set(&Self::floating_ip_alloc_v4_key(v4), b"");
                        }
                        std::net::IpAddr::V6(v6) => {
                            tr.set(&Self::floating_ip_alloc_v6_key(v6), b"");
                        }
                    }
                    Ok(Outcome::Created(Box::new(fip)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(fip)) => Ok(*fip),
            Ok(Outcome::ProjectMissingOrWrongSilo) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "floating ip with name {:?} already exists in project {project_id}",
                req.name
            ))),
            Ok(Outcome::PoolExhausted) => Err(StoreError::Backend(
                "floating ip pool exhausted".to_string(),
            )),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_floating_ip(&self, fip_id: Uuid) -> Result<FloatingIp, StoreError> {
        let key = Self::floating_ip_by_id_key(fip_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize floating ip: {e}")))
    }

    async fn list_floating_ips_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<FloatingIp>, StoreError> {
        let prefix = Self::floating_ip_in_project_prefix(project_id);
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
                .map_err(|e| StoreError::Backend(format!("floating ip index uuid: {e}")))?;
            let by_id_key = Self::floating_ip_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let fip: FloatingIp = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize floating ip: {e}")))?;
                out.push(fip);
            }
        }
        Ok(out)
    }

    async fn delete_floating_ip(&self, fip_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = Self::floating_ip_by_id_key(fip_id);

        enum Out {
            Deleted,
            Vanished,
            Attached,
        }
        let outcome: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Out::Vanished),
                    };
                    let fip: FloatingIp = match serde_json::from_slice(&bytes) {
                        Ok(f) => f,
                        Err(_) => return Ok(Out::Vanished),
                    };
                    if fip.attached_to.is_some() {
                        return Ok(Out::Attached);
                    }
                    let by_name_key =
                        format!("floating_ip/by_project/{}/{}", fip.project_id, fip.name)
                            .into_bytes();
                    let in_project_key =
                        format!("floating_ip/in_project/{}/{}", fip.project_id, fip.id)
                            .into_bytes();
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_project_key);
                    match fip.address {
                        std::net::IpAddr::V4(v4) => {
                            tr.clear(&Self::floating_ip_alloc_v4_key(v4));
                        }
                        std::net::IpAddr::V6(v6) => {
                            tr.clear(&Self::floating_ip_alloc_v6_key(v6));
                        }
                    }
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) => Ok(()),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::Attached) => Err(StoreError::Conflict(format!(
                "floating ip {fip_id} is currently attached; detach first"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn attach_floating_ip(
        &self,
        fip_id: Uuid,
        target_nic_id: Uuid,
    ) -> Result<FloatingIp, StoreError> {
        let by_id_key = Self::floating_ip_by_id_key(fip_id);
        let nic_check_key = Self::nic_by_id_key(target_nic_id);

        enum Out {
            Attached(Box<FloatingIp>),
            FipMissing,
            NicMissingOrWrongParent,
        }
        let outcome: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let nic_check_key = nic_check_key.clone();
                async move {
                    let fip_bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Out::FipMissing),
                    };
                    let mut fip: FloatingIp = match serde_json::from_slice(&fip_bytes) {
                        Ok(f) => f,
                        Err(_) => return Ok(Out::FipMissing),
                    };
                    let nic_bytes = match tr.get(&nic_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Out::NicMissingOrWrongParent),
                    };
                    let nic: Nic = match serde_json::from_slice(&nic_bytes) {
                        Ok(n) => n,
                        Err(_) => return Ok(Out::NicMissingOrWrongParent),
                    };
                    if nic.silo_id != fip.silo_id || nic.project_id != fip.project_id {
                        return Ok(Out::NicMissingOrWrongParent);
                    }
                    fip.attached_to = Some(FloatingIpAttachment {
                        instance_id: nic.instance_id,
                        nic_id: target_nic_id,
                        attached_at: Utc::now(),
                    });
                    fip.updated_at = Utc::now();
                    let value = match serde_json::to_vec(&fip) {
                        Ok(v) => v,
                        Err(_) => return Ok(Out::FipMissing),
                    };
                    tr.set(&by_id_key, &value);
                    Ok(Out::Attached(Box::new(fip)))
                }
            })
            .await;

        match outcome {
            Ok(Out::Attached(fip)) => Ok(*fip),
            Ok(Out::FipMissing) | Ok(Out::NicMissingOrWrongParent) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn detach_floating_ip(&self, fip_id: Uuid) -> Result<FloatingIp, StoreError> {
        let by_id_key = Self::floating_ip_by_id_key(fip_id);

        enum Out {
            Detached(Box<FloatingIp>),
            Vanished,
        }
        let outcome: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Out::Vanished),
                    };
                    let mut fip: FloatingIp = match serde_json::from_slice(&bytes) {
                        Ok(f) => f,
                        Err(_) => return Ok(Out::Vanished),
                    };
                    fip.attached_to = None;
                    fip.updated_at = Utc::now();
                    let value = match serde_json::to_vec(&fip) {
                        Ok(v) => v,
                        Err(_) => return Ok(Out::Vanished),
                    };
                    tr.set(&by_id_key, &value);
                    Ok(Out::Detached(Box::new(fip)))
                }
            })
            .await;

        match outcome {
            Ok(Out::Detached(fip)) => Ok(*fip),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
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
                let target_cn_uuid = req.target_cn_uuid;
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
                        target_cn_uuid,
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

    async fn list_stale_claims(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ProvisioningJob>, StoreError> {
        // Scan every job and filter — Phase 0 has no by-status
        // index. The hot path is `claim_next_job` (which does
        // have its own pending index); the sweeper runs once a
        // minute and queue sizes are small enough that a full
        // scan is cheap.
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

        let stale = raws
            .into_iter()
            .filter_map(|b| serde_json::from_slice::<ProvisioningJob>(&b).ok())
            .filter(|j| matches!(j.status.kind(), JobStatusKind::InProgress))
            .filter(|j| j.claimed_at.is_some_and(|t| t < cutoff))
            .collect();
        Ok(stale)
    }

    async fn claim_next_job(
        &self,
        claimed_by: &str,
        claimer_cn: Option<Uuid>,
    ) -> Result<ProvisioningJob, StoreError> {
        let prefix = Self::job_pending_prefix().to_vec();
        let (begin, end) = prefix_range(&prefix);
        let claimed_by = claimed_by.to_string();

        enum Outcome {
            Claimed(Box<ProvisioningJob>),
            Empty,
        }

        // Linear walk over Pending in seq order; skip mis-targeted
        // jobs. Cap at 256 so a flood of routed jobs an unbound
        // claimer can't take doesn't turn into a degenerate scan.
        const SCAN_LIMIT: usize = 256;

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
                        limit: Some(SCAN_LIMIT),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    let entries: Vec<(Vec<u8>, Vec<u8>)> = kvs
                        .iter()
                        .map(|kv| (kv.key().to_vec(), kv.value().to_vec()))
                        .collect();
                    drop(kvs);

                    for (pending_key, job_id_bytes) in entries {
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
                            None => {
                                // Pending index points at a vanished
                                // record; clear and continue to the
                                // next candidate.
                                tr.clear(&pending_key);
                                continue;
                            }
                        };
                        let mut job: ProvisioningJob =
                            serde_json::from_slice(&bytes).map_err(|e| {
                                FdbBindingError::CustomError(format!("deserialize job: {e}").into())
                            })?;
                        // Targeting check: skip mis-routed jobs
                        // without clearing their pending index — a
                        // different agent will pick them up.
                        if !targeting_matches(job.target_cn_uuid, claimer_cn) {
                            continue;
                        }
                        job.status = JobStatus::InProgress;
                        job.claimed_at = Some(Utc::now());
                        job.claimed_by = Some(claimed_by);
                        let value = serde_json::to_vec(&job).map_err(|e| {
                            FdbBindingError::CustomError(format!("serialize job: {e}").into())
                        })?;
                        tr.set(&by_id_key, &value);
                        tr.clear(&pending_key);
                        return Ok(Outcome::Claimed(Box::new(job)));
                    }
                    Ok(Outcome::Empty)
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

    async fn register_cn(
        &self,
        server_uuid: Uuid,
        hostname: String,
        admin_ip: Option<std::net::Ipv4Addr>,
        sysinfo: serde_json::Value,
        now: chrono::DateTime<Utc>,
    ) -> Result<Cn, StoreError> {
        // Outcome carries the resulting Cn (or a logical conflict)
        // out of the FDB transaction closure.
        enum Outcome {
            Created(Box<Cn>),
            Disabled,
            ClaimCodeExhausted,
        }

        let by_uuid_key = Self::cn_by_uuid_key(server_uuid);
        let window_key = Self::auto_approve_window_key().to_vec();
        let server_uuid_bytes = server_uuid.to_string().into_bytes();

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                let window_key = window_key.clone();
                let server_uuid_bytes = server_uuid_bytes.clone();
                let hostname = hostname.clone();
                let sysinfo = sysinfo.clone();
                async move {
                    // Branch 1: existing record at by_uuid.
                    if let Some(bytes) = tr.get(&by_uuid_key, false).await? {
                        let existing: Cn = serde_json::from_slice(&bytes).map_err(|e| {
                            FdbBindingError::CustomError(format!("deserialize cn: {e}").into())
                        })?;
                        match existing.state {
                            CnState::Disabled => {
                                return Ok(Outcome::Disabled);
                            }
                            CnState::Approved => {
                                // Idempotent refresh: keep credentials,
                                // refresh sysinfo + hostname + last_seen.
                                let mut updated = existing;
                                updated.hostname = hostname;
                                updated.admin_ip = admin_ip;
                                updated.sysinfo = sysinfo;
                                updated.last_seen = Some(now);
                                let value = serde_json::to_vec(&updated).map_err(|e| {
                                    FdbBindingError::CustomError(
                                        format!("serialize cn: {e}").into(),
                                    )
                                })?;
                                tr.set(&by_uuid_key, &value);
                                return Ok(Outcome::Created(Box::new(updated)));
                            }
                            CnState::Pending => {
                                // Drop the old by_claim and by_poll
                                // index entries; mint fresh ones (with
                                // collision check).
                                if let Some(old_code) = &existing.claim_code {
                                    tr.clear(&Self::cn_by_claim_key(old_code));
                                }
                                tr.clear(&Self::cn_by_poll_key(&existing.poll_token));

                                let (claim_code, poll_token) =
                                    match mint_unique_claim_and_poll(&tr).await? {
                                        Some(pair) => pair,
                                        None => return Ok(Outcome::ClaimCodeExhausted),
                                    };
                                let cn = Cn {
                                    server_uuid,
                                    hostname,
                                    admin_ip,
                                    state: CnState::Pending,
                                    registered_at: existing.registered_at,
                                    approved_at: None,
                                    last_seen: Some(now),
                                    sysinfo,
                                    claim_code: Some(claim_code.clone()),
                                    claim_code_expires_at: Some(now + claim_code_ttl()),
                                    poll_token: poll_token.clone(),
                                    bound_api_key_id: None,
                                    pending_credential: None,
                                    last_status: None,
                                };
                                let value = serde_json::to_vec(&cn).map_err(|e| {
                                    FdbBindingError::CustomError(
                                        format!("serialize cn: {e}").into(),
                                    )
                                })?;
                                tr.set(&by_uuid_key, &value);
                                tr.set(&Self::cn_by_claim_key(&claim_code), &server_uuid_bytes);
                                tr.set(&Self::cn_by_poll_key(&poll_token), &server_uuid_bytes);
                                // by_state membership is unchanged (still pending).
                                tr.set(&Self::cn_by_state_key(CnState::Pending, server_uuid), b"");
                                return Ok(Outcome::Created(Box::new(cn)));
                            }
                        }
                    }

                    // Branch 2: brand-new registration. Try to consume
                    // an auto-approve slot atomically inside this txn.
                    let auto_approved =
                        consume_auto_approve_slot_in_txn(&tr, &window_key, now).await?;

                    let poll_token = match mint_unique_poll_token(&tr).await? {
                        Some(t) => t,
                        None => return Ok(Outcome::ClaimCodeExhausted),
                    };
                    let (claim_code, claim_expiry, state) = if auto_approved {
                        (None, None, CnState::Approved)
                    } else {
                        let code = match mint_unique_claim_code(&tr).await? {
                            Some(c) => c,
                            None => return Ok(Outcome::ClaimCodeExhausted),
                        };
                        let expiry = now + claim_code_ttl();
                        (Some(code), Some(expiry), CnState::Pending)
                    };

                    let cn = Cn {
                        server_uuid,
                        hostname,
                        admin_ip,
                        state,
                        registered_at: now,
                        approved_at: if auto_approved { Some(now) } else { None },
                        last_seen: Some(now),
                        sysinfo,
                        claim_code: claim_code.clone(),
                        claim_code_expires_at: claim_expiry,
                        poll_token: poll_token.clone(),
                        bound_api_key_id: None,
                        pending_credential: None,
                        last_status: None,
                    };
                    let value = serde_json::to_vec(&cn).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize cn: {e}").into())
                    })?;
                    tr.set(&by_uuid_key, &value);
                    if let Some(code) = &claim_code {
                        tr.set(&Self::cn_by_claim_key(code), &server_uuid_bytes);
                    }
                    tr.set(&Self::cn_by_poll_key(&poll_token), &server_uuid_bytes);
                    tr.set(&Self::cn_by_state_key(state, server_uuid), b"");
                    Ok(Outcome::Created(Box::new(cn)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(cn)) => Ok(*cn),
            Ok(Outcome::Disabled) => Err(StoreError::Conflict(format!(
                "cn {server_uuid} is disabled; remove the record before re-registering"
            ))),
            Ok(Outcome::ClaimCodeExhausted) => {
                Err(StoreError::Backend("claim code exhausted".to_string()))
            }
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_cn(&self, server_uuid: Uuid) -> Result<Cn, StoreError> {
        let key = Self::cn_by_uuid_key(server_uuid);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize cn: {e}")))
    }

    async fn get_cn_by_poll_token(&self, poll_token: &str) -> Result<Cn, StoreError> {
        let key = Self::cn_by_poll_key(poll_token);
        let id_bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("cn poll index not utf8: {e}")))?;
        let id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("cn poll index not uuid: {e}")))?;
        self.get_cn(id).await
    }

    async fn get_cn_by_claim_code(&self, code: &str) -> Result<Cn, StoreError> {
        let key = Self::cn_by_claim_key(code);
        let id_bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("cn claim index not utf8: {e}")))?;
        let id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("cn claim index not uuid: {e}")))?;
        let cn = self.get_cn(id).await?;
        // Conflate state-mismatch and expiry into NotFound so probes
        // can't distinguish "wrong code" from "right code, wrong state".
        if cn.state != CnState::Pending {
            return Err(StoreError::NotFound);
        }
        if let Some(expiry) = cn.claim_code_expires_at
            && Utc::now() >= expiry
        {
            return Err(StoreError::NotFound);
        }
        Ok(cn)
    }

    async fn list_cns(&self, state_filter: Option<CnState>) -> Result<Vec<Cn>, StoreError> {
        let states: Vec<CnState> = match state_filter {
            Some(s) => vec![s],
            None => vec![CnState::Pending, CnState::Approved, CnState::Disabled],
        };

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for state in states {
            let prefix = Self::cn_by_state_prefix(state);
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
            let id_strs =
                id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

            for s in id_strs {
                let id = Uuid::parse_str(&s)
                    .map_err(|e| StoreError::Backend(format!("cn state index uuid: {e}")))?;
                if !seen.insert(id) {
                    continue;
                }
                let by_id_key = Self::cn_by_uuid_key(id);
                if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                    let cn: Cn = serde_json::from_slice(&bytes)
                        .map_err(|e| StoreError::Backend(format!("deserialize cn: {e}")))?;
                    out.push(cn);
                }
            }
        }
        Ok(out)
    }

    async fn approve_cn(
        &self,
        server_uuid: Uuid,
        bound_api_key_id: Uuid,
        pending_credential: String,
        approved_at: chrono::DateTime<Utc>,
    ) -> Result<Cn, StoreError> {
        enum Outcome {
            Approved(Box<Cn>),
            NotFound,
            AlreadyBound,
        }

        let by_uuid_key = Self::cn_by_uuid_key(server_uuid);
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                let pending_credential = pending_credential.clone();
                async move {
                    let bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(|e| {
                        FdbBindingError::CustomError(format!("deserialize cn: {e}").into())
                    })?;
                    // Disabled: treat as gone.
                    if cn.state == CnState::Disabled {
                        return Ok(Outcome::NotFound);
                    }
                    // Already bound: programmer error — never re-mint
                    // without going through disable first.
                    if cn.bound_api_key_id.is_some() {
                        return Ok(Outcome::AlreadyBound);
                    }
                    let prev_state = cn.state;
                    if let Some(old_code) = &cn.claim_code {
                        tr.clear(&Self::cn_by_claim_key(old_code));
                    }
                    // If the record was Pending, drop the by_state/pending
                    // membership before adding the new approved one.
                    // (Auto-approve case: register_cn already wrote
                    // by_state/approved, so this is a no-op clear.)
                    tr.clear(&Self::cn_by_state_key(prev_state, server_uuid));

                    cn.state = CnState::Approved;
                    // Preserve approved_at when register_cn already
                    // stamped it (auto-approve case); set it now for
                    // the Pending → Approved transition.
                    if cn.approved_at.is_none() {
                        cn.approved_at = Some(approved_at);
                    }
                    cn.claim_code = None;
                    cn.claim_code_expires_at = None;
                    cn.bound_api_key_id = Some(bound_api_key_id);
                    cn.pending_credential = Some(pending_credential);

                    let value = serde_json::to_vec(&cn).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize cn: {e}").into())
                    })?;
                    tr.set(&by_uuid_key, &value);
                    tr.set(&Self::cn_by_state_key(CnState::Approved, server_uuid), b"");
                    Ok(Outcome::Approved(Box::new(cn)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Approved(cn)) => Ok(*cn),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Ok(Outcome::AlreadyBound) => Err(StoreError::Conflict(
                "cn already has a bound api key; disable + re-approve to rotate".to_string(),
            )),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn consume_cn_pending_credential(
        &self,
        poll_token: &str,
    ) -> Result<Option<String>, StoreError> {
        let by_poll_key = Self::cn_by_poll_key(poll_token);

        enum Outcome {
            Consumed(Option<String>),
            NotFound,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_poll_key = by_poll_key.clone();
                async move {
                    let id_bytes = match tr.get(&by_poll_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let id_str = std::str::from_utf8(&id_bytes).map_err(|e| {
                        FdbBindingError::CustomError(format!("cn poll index not utf8: {e}").into())
                    })?;
                    let id = Uuid::parse_str(id_str).map_err(|e| {
                        FdbBindingError::CustomError(format!("cn poll index not uuid: {e}").into())
                    })?;
                    let by_uuid_key = Self::cn_by_uuid_key(id);
                    let cn_bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&cn_bytes).map_err(|e| {
                        FdbBindingError::CustomError(format!("deserialize cn: {e}").into())
                    })?;
                    let taken = cn.pending_credential.take();
                    if taken.is_some() {
                        let value = serde_json::to_vec(&cn).map_err(|e| {
                            FdbBindingError::CustomError(format!("serialize cn: {e}").into())
                        })?;
                        tr.set(&by_uuid_key, &value);
                    }
                    Ok(Outcome::Consumed(taken))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Consumed(opt)) => Ok(opt),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn disable_cn(&self, server_uuid: Uuid) -> Result<Cn, StoreError> {
        enum Outcome {
            Disabled(Box<Cn>),
            NotFound,
        }

        let by_uuid_key = Self::cn_by_uuid_key(server_uuid);
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                async move {
                    let bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(|e| {
                        FdbBindingError::CustomError(format!("deserialize cn: {e}").into())
                    })?;
                    if let Some(old_code) = &cn.claim_code {
                        tr.clear(&Self::cn_by_claim_key(old_code));
                    }
                    let old_state = cn.state;
                    tr.clear(&Self::cn_by_state_key(old_state, server_uuid));

                    cn.state = CnState::Disabled;
                    cn.claim_code = None;
                    cn.claim_code_expires_at = None;
                    cn.pending_credential = None;

                    let value = serde_json::to_vec(&cn).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize cn: {e}").into())
                    })?;
                    tr.set(&by_uuid_key, &value);
                    tr.set(&Self::cn_by_state_key(CnState::Disabled, server_uuid), b"");
                    Ok(Outcome::Disabled(Box::new(cn)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Disabled(cn)) => Ok(*cn),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn update_cn_last_seen(
        &self,
        server_uuid: Uuid,
        at: chrono::DateTime<Utc>,
    ) -> Result<(), StoreError> {
        enum Outcome {
            Updated,
            NotFound,
        }

        let by_uuid_key = Self::cn_by_uuid_key(server_uuid);
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                async move {
                    let bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(|e| {
                        FdbBindingError::CustomError(format!("deserialize cn: {e}").into())
                    })?;
                    cn.last_seen = Some(at);
                    let value = serde_json::to_vec(&cn).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize cn: {e}").into())
                    })?;
                    tr.set(&by_uuid_key, &value);
                    Ok(Outcome::Updated)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated) => Ok(()),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn update_cn_status(
        &self,
        server_uuid: Uuid,
        payload: serde_json::Value,
        at: chrono::DateTime<Utc>,
    ) -> Result<(), StoreError> {
        enum Outcome {
            Updated,
            NotFound,
        }

        let by_uuid_key = Self::cn_by_uuid_key(server_uuid);
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                let payload = payload.clone();
                async move {
                    let bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(|e| {
                        FdbBindingError::CustomError(format!("deserialize cn: {e}").into())
                    })?;
                    cn.last_status = Some(payload);
                    cn.last_seen = Some(at);
                    let value = serde_json::to_vec(&cn).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize cn: {e}").into())
                    })?;
                    tr.set(&by_uuid_key, &value);
                    Ok(Outcome::Updated)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated) => Ok(()),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_auto_approve_window(&self) -> Result<Option<AutoApproveWindow>, StoreError> {
        let key = Self::auto_approve_window_key().to_vec();
        let bytes = self.read_bytes(&key).await?;
        match bytes {
            Some(bytes) => {
                let w: AutoApproveWindow = serde_json::from_slice(&bytes).map_err(|e| {
                    StoreError::Backend(format!("deserialize auto-approve window: {e}"))
                })?;
                Ok(Some(w))
            }
            None => Ok(None),
        }
    }

    async fn open_auto_approve_window(&self, w: AutoApproveWindow) -> Result<(), StoreError> {
        let value = serde_json::to_vec(&w)
            .map_err(|e| StoreError::Backend(format!("serialize auto-approve window: {e}")))?;
        let key = Self::auto_approve_window_key().to_vec();
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
        result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))
    }

    async fn close_auto_approve_window(&self) -> Result<(), StoreError> {
        let key = Self::auto_approve_window_key().to_vec();
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move {
                    tr.clear(&key);
                    Ok(())
                }
            })
            .await;
        result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))
    }

    async fn try_consume_auto_approve_slot(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let key = Self::auto_approve_window_key().to_vec();
        let result: Result<bool, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { consume_auto_approve_slot_in_txn(&tr, &key, now).await }
            })
            .await;
        result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))
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

/// `chrono::Duration` form of [`CLAIM_CODE_TTL`]. Hand-converted (the
/// public constant is `std::time::Duration`) without `.expect()` so
/// the workspace `clippy::expect_used` deny lint is happy. The TTL
/// is one hour; well within `chrono::Duration`'s i64-millis range.
fn claim_code_ttl() -> chrono::Duration {
    chrono::Duration::seconds(CLAIM_CODE_TTL.as_secs() as i64)
}

/// Number of attempts to mint a fresh, collision-free claim code or
/// poll token inside a transaction. With 30 bits of claim entropy and
/// 128 bits of poll entropy, collision is vanishingly rare; the cap is
/// purely defensive.
const CN_CODE_RETRY_ATTEMPTS: usize = 16;

/// Mint a claim code that does not already index any CN. Returns
/// `None` if every attempt collided (treated by the caller as a
/// `Backend("claim code exhausted")`).
async fn mint_unique_claim_code(
    tr: &foundationdb::RetryableTransaction,
) -> Result<Option<String>, FdbBindingError> {
    for _ in 0..CN_CODE_RETRY_ATTEMPTS {
        let code = {
            let mut rng = rand::rng();
            generate_claim_code(&mut rng)
        };
        let key = format!("cn/by_claim/{code}").into_bytes();
        if tr.get(&key, false).await?.is_none() {
            return Ok(Some(code));
        }
    }
    Ok(None)
}

/// Mint a poll token that does not already index any CN.
async fn mint_unique_poll_token(
    tr: &foundationdb::RetryableTransaction,
) -> Result<Option<String>, FdbBindingError> {
    for _ in 0..CN_CODE_RETRY_ATTEMPTS {
        let token = {
            let mut rng = rand::rng();
            generate_poll_token(&mut rng)
        };
        let key = format!("cn/by_poll/{token}").into_bytes();
        if tr.get(&key, false).await?.is_none() {
            return Ok(Some(token));
        }
    }
    Ok(None)
}

/// Convenience for the Pending-rotation path: mint a fresh claim
/// code + poll token pair, both checked against their indexes.
async fn mint_unique_claim_and_poll(
    tr: &foundationdb::RetryableTransaction,
) -> Result<Option<(String, String)>, FdbBindingError> {
    let Some(claim) = mint_unique_claim_code(tr).await? else {
        return Ok(None);
    };
    let Some(poll) = mint_unique_poll_token(tr).await? else {
        return Ok(None);
    };
    Ok(Some((claim, poll)))
}

/// Atomically: read the auto-approve window singleton; if it's open,
/// unexpired, and has slot, decrement (or close on exhaust) and
/// return true. Otherwise return false. Shared between
/// `register_cn`'s auto-approve path and `try_consume_auto_approve_slot`.
async fn consume_auto_approve_slot_in_txn(
    tr: &foundationdb::RetryableTransaction,
    window_key: &[u8],
    now: chrono::DateTime<Utc>,
) -> Result<bool, FdbBindingError> {
    let bytes = match tr.get(window_key, false).await? {
        Some(b) => b,
        None => return Ok(false),
    };
    let mut window: AutoApproveWindow = serde_json::from_slice(&bytes).map_err(|e| {
        FdbBindingError::CustomError(format!("deserialize auto-approve window: {e}").into())
    })?;
    if now >= window.expires_at {
        tr.clear(window_key);
        return Ok(false);
    }
    match window.remaining_count {
        Some(0) => {
            tr.clear(window_key);
            Ok(false)
        }
        Some(ref mut n) => {
            *n -= 1;
            let exhausted = *n == 0;
            if exhausted {
                tr.clear(window_key);
            } else {
                let value = serde_json::to_vec(&window).map_err(|e| {
                    FdbBindingError::CustomError(
                        format!("serialize auto-approve window: {e}").into(),
                    )
                })?;
                tr.set(window_key, &value);
            }
            Ok(true)
        }
        None => Ok(true),
    }
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

#[cfg(test)]
mod cn_tests {
    //! CN registration / approval tests against a real FoundationDB
    //! cluster. Mirrors the MemStore CN test block in `mem.rs`.
    //!
    //! Marked `#[ignore]` because they require a running FDB cluster
    //! reachable via the default cluster file resolution. Run with
    //! `cargo test -p tritond-store --features foundationdb -- --ignored`.
    //!
    //! Each test uses a per-test key prefix (via a random uuid in the
    //! `server_uuid`) so concurrent runs don't trip on each other; we
    //! do not blow away the keyspace.
    use super::*;
    use crate::AutoApproveWindow;

    fn fdb_test_store() -> FdbStore {
        FdbStore::open(None).expect("open FDB cluster from default cluster file")
    }

    fn sysinfo_fixture() -> serde_json::Value {
        serde_json::json!({
            "UUID": "00000000-0000-0000-0000-000000000001",
            "Hostname": "test-cn",
        })
    }

    /// Drop every key the CN tests touch for `server_uuid`. Runs at
    /// the start of each test so reruns against a stale FDB cluster
    /// produce repeatable state.
    async fn purge_cn(store: &FdbStore, server_uuid: Uuid) {
        // Read the record first to learn which claim/poll indices
        // need clearing; ignore any decode failure.
        let by_uuid = FdbStore::cn_by_uuid_key(server_uuid);
        if let Ok(Some(bytes)) = store.read_bytes(&by_uuid).await
            && let Ok(cn) = serde_json::from_slice::<Cn>(&bytes)
        {
            let _ = store
                .db
                .run(|tr, _| {
                    let by_uuid = by_uuid.clone();
                    let claim_key = cn.claim_code.as_deref().map(FdbStore::cn_by_claim_key);
                    let poll_key = FdbStore::cn_by_poll_key(&cn.poll_token);
                    let state_key = FdbStore::cn_by_state_key(cn.state, server_uuid);
                    let pending_state_key =
                        FdbStore::cn_by_state_key(CnState::Pending, server_uuid);
                    let approved_state_key =
                        FdbStore::cn_by_state_key(CnState::Approved, server_uuid);
                    let disabled_state_key =
                        FdbStore::cn_by_state_key(CnState::Disabled, server_uuid);
                    async move {
                        tr.clear(&by_uuid);
                        if let Some(k) = claim_key.as_deref() {
                            tr.clear(k);
                        }
                        tr.clear(&poll_key);
                        tr.clear(&state_key);
                        // Belt-and-suspenders: clear all three state
                        // membership keys in case state was rewritten
                        // between the read above and this txn.
                        tr.clear(&pending_state_key);
                        tr.clear(&approved_state_key);
                        tr.clear(&disabled_state_key);
                        Ok(())
                    }
                })
                .await;
        } else {
            // Best-effort clear of state membership rows even with no
            // cn record (lets stuck rows from prior runs go away).
            let _ = store
                .db
                .run(|tr, _| {
                    let pending = FdbStore::cn_by_state_key(CnState::Pending, server_uuid);
                    let approved = FdbStore::cn_by_state_key(CnState::Approved, server_uuid);
                    let disabled = FdbStore::cn_by_state_key(CnState::Disabled, server_uuid);
                    async move {
                        tr.clear(&pending);
                        tr.clear(&approved);
                        tr.clear(&disabled);
                        Ok(())
                    }
                })
                .await;
        }
    }

    /// Clear the auto-approve singleton. Used by every auto-approve
    /// test so leftover state from a previous run doesn't leak in.
    async fn purge_window(store: &FdbStore) {
        let _ = store.close_auto_approve_window().await;
    }

    #[tokio::test]
    #[ignore]
    async fn register_cn_creates_pending_with_claim_code() {
        let store = fdb_test_store();
        let id = Uuid::new_v4();
        purge_cn(&store, id).await;
        purge_window(&store).await;

        let now = Utc::now();
        let cn = store
            .register_cn(id, "host1".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register_cn");
        assert_eq!(cn.state, CnState::Pending);
        assert!(cn.claim_code.is_some());
        assert_eq!(cn.claim_code.as_ref().expect("claim").len(), 6);
        assert_eq!(cn.poll_token.len(), 32);
        assert!(cn.bound_api_key_id.is_none());
        assert!(cn.pending_credential.is_none());
        assert!(cn.approved_at.is_none());

        purge_cn(&store, id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn re_register_pending_rotates_claim_code() {
        let store = fdb_test_store();
        let id = Uuid::new_v4();
        purge_cn(&store, id).await;
        purge_window(&store).await;

        let now = Utc::now();
        let first = store
            .register_cn(id, "host1".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register first");
        let second = store
            .register_cn(id, "host1-renamed".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register second");
        assert_eq!(first.registered_at, second.registered_at);
        assert_ne!(first.claim_code, second.claim_code);
        assert_ne!(first.poll_token, second.poll_token);
        assert_eq!(second.hostname, "host1-renamed");

        let err = store
            .get_cn_by_claim_code(first.claim_code.as_ref().expect("claim"))
            .await
            .expect_err("old claim should be unfindable");
        assert!(matches!(err, StoreError::NotFound));

        purge_cn(&store, id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn re_register_approved_is_idempotent() {
        let store = fdb_test_store();
        let id = Uuid::new_v4();
        purge_cn(&store, id).await;
        purge_window(&store).await;

        let now = Utc::now();
        store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register");
        store
            .approve_cn(id, Uuid::new_v4(), "tcadm_xxx".into(), now)
            .await
            .expect("approve");

        let later = now + chrono::Duration::seconds(60);
        let updated = store
            .register_cn(
                id,
                "h2".into(),
                None,
                serde_json::json!({"updated": true}),
                later,
            )
            .await
            .expect("re-register");
        assert_eq!(updated.state, CnState::Approved);
        assert_eq!(updated.hostname, "h2");
        assert_eq!(updated.last_seen, Some(later));
        assert_eq!(updated.sysinfo, serde_json::json!({"updated": true}));

        purge_cn(&store, id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn approve_cn_flips_state_and_stashes_credential() {
        let store = fdb_test_store();
        let id = Uuid::new_v4();
        purge_cn(&store, id).await;
        purge_window(&store).await;

        let now = Utc::now();
        let cn = store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register");
        let key_id = Uuid::new_v4();
        let approved = store
            .approve_cn(id, key_id, "tcadm_secret".into(), now)
            .await
            .expect("approve");
        assert_eq!(approved.state, CnState::Approved);
        assert!(approved.claim_code.is_none());
        assert_eq!(approved.bound_api_key_id, Some(key_id));
        assert_eq!(approved.pending_credential.as_deref(), Some("tcadm_secret"));

        let err = store
            .get_cn_by_claim_code(cn.claim_code.as_ref().expect("claim"))
            .await
            .expect_err("old claim should be unfindable");
        assert!(matches!(err, StoreError::NotFound));

        let consumed = store
            .consume_cn_pending_credential(&cn.poll_token)
            .await
            .expect("consume first");
        assert_eq!(consumed.as_deref(), Some("tcadm_secret"));

        let consumed_again = store
            .consume_cn_pending_credential(&cn.poll_token)
            .await
            .expect("consume second");
        assert!(consumed_again.is_none());

        purge_cn(&store, id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn approve_cn_pending_only() {
        let store = fdb_test_store();
        let id = Uuid::new_v4();
        purge_cn(&store, id).await;

        let err = store
            .approve_cn(id, Uuid::new_v4(), "x".into(), Utc::now())
            .await
            .expect_err("approve before register should fail");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    #[ignore]
    async fn disable_cn_blocks_re_registration() {
        let store = fdb_test_store();
        let id = Uuid::new_v4();
        purge_cn(&store, id).await;
        purge_window(&store).await;

        let now = Utc::now();
        store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register");
        store.disable_cn(id).await.expect("disable");
        let err = store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
            .await
            .expect_err("re-register after disable should fail");
        assert!(matches!(err, StoreError::Conflict(_)));

        purge_cn(&store, id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn auto_approve_window_promotes_registration() {
        let store = fdb_test_store();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        purge_cn(&store, id1).await;
        purge_cn(&store, id2).await;
        purge_cn(&store, id3).await;
        purge_window(&store).await;

        let now = Utc::now();
        store
            .open_auto_approve_window(AutoApproveWindow {
                opened_at: now,
                expires_at: now + chrono::Duration::minutes(30),
                remaining_count: Some(2),
                opened_by: "root".into(),
            })
            .await
            .expect("open window");

        let cn1 = store
            .register_cn(id1, "h1".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register cn1");
        assert_eq!(cn1.state, CnState::Approved);
        assert!(cn1.claim_code.is_none());
        assert!(cn1.approved_at.is_some());

        let cn2 = store
            .register_cn(id2, "h2".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register cn2");
        assert_eq!(cn2.state, CnState::Approved);

        let cn3 = store
            .register_cn(id3, "h3".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register cn3");
        assert_eq!(cn3.state, CnState::Pending);
        assert!(cn3.claim_code.is_some());

        assert!(
            store
                .get_auto_approve_window()
                .await
                .expect("get window")
                .is_none()
        );

        purge_cn(&store, id1).await;
        purge_cn(&store, id2).await;
        purge_cn(&store, id3).await;
    }

    #[tokio::test]
    #[ignore]
    async fn auto_approve_window_expires_on_time() {
        let store = fdb_test_store();
        let id = Uuid::new_v4();
        purge_cn(&store, id).await;
        purge_window(&store).await;

        let opened = Utc::now();
        store
            .open_auto_approve_window(AutoApproveWindow {
                opened_at: opened,
                expires_at: opened + chrono::Duration::seconds(10),
                remaining_count: None,
                opened_by: "root".into(),
            })
            .await
            .expect("open window");

        let later = opened + chrono::Duration::seconds(20);
        let cn = store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), later)
            .await
            .expect("register");
        assert_eq!(cn.state, CnState::Pending);
        assert!(
            store
                .get_auto_approve_window()
                .await
                .expect("get window")
                .is_none()
        );

        purge_cn(&store, id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn list_cns_filters_by_state() {
        let store = fdb_test_store();
        let pid = Uuid::new_v4();
        let aid = Uuid::new_v4();
        purge_cn(&store, pid).await;
        purge_cn(&store, aid).await;
        purge_window(&store).await;

        let now = Utc::now();
        store
            .register_cn(pid, "p".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register pending");
        store
            .register_cn(aid, "a".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register approved-target");
        store
            .approve_cn(aid, Uuid::new_v4(), "k".into(), now)
            .await
            .expect("approve");

        let pending = store
            .list_cns(Some(CnState::Pending))
            .await
            .expect("list pending");
        assert!(pending.iter().any(|c| c.server_uuid == pid));
        assert!(!pending.iter().any(|c| c.server_uuid == aid));

        let approved = store
            .list_cns(Some(CnState::Approved))
            .await
            .expect("list approved");
        assert!(approved.iter().any(|c| c.server_uuid == aid));
        assert!(!approved.iter().any(|c| c.server_uuid == pid));

        let all = store.list_cns(None).await.expect("list all");
        assert!(all.iter().any(|c| c.server_uuid == pid));
        assert!(all.iter().any(|c| c.server_uuid == aid));

        purge_cn(&store, pid).await;
        purge_cn(&store, aid).await;
    }
}

#[cfg(test)]
mod tenant_tests {
    //! Tenant CRUD tests against a real FoundationDB cluster. Mirrors
    //! the MemStore tenant test block in `mem.rs`.
    //!
    //! Marked `#[ignore]` because they require a running FDB cluster
    //! reachable via the default cluster file resolution. Run with
    //! `cargo test -p tritond-store --features foundationdb -- --ignored`.
    //!
    //! Each test mints fresh silo + tenant uuids so concurrent runs
    //! against a shared cluster don't trip on each other; we do not
    //! blow away the keyspace.
    use super::*;

    fn fdb_test_store() -> FdbStore {
        FdbStore::open(None).expect("open FDB cluster from default cluster file")
    }

    /// Drop a tenant row + indices we know about. Best-effort — the
    /// row may have been deleted by the test itself.
    async fn purge_tenant(store: &FdbStore, tenant_id: Uuid) {
        let by_id = FdbStore::tenant_by_id_key(tenant_id);
        if let Ok(Some(bytes)) = store.read_bytes(&by_id).await
            && let Ok(t) = serde_json::from_slice::<Tenant>(&bytes)
        {
            let by_name = FdbStore::tenant_by_silo_name_key(t.silo_id, &t.name);
            let in_silo = FdbStore::tenant_in_silo_key(t.silo_id, t.id);
            let _ = store
                .db
                .run(|tr, _| {
                    let by_id = by_id.clone();
                    let by_name = by_name.clone();
                    let in_silo = in_silo.clone();
                    async move {
                        tr.clear(&by_id);
                        tr.clear(&by_name);
                        tr.clear(&in_silo);
                        Ok(())
                    }
                })
                .await;
        }
    }

    /// Drop a silo row + by_name index, plus the default tenant
    /// that was created atomically with the silo. Best-effort cleanup.
    async fn purge_silo(store: &FdbStore, silo_id: Uuid) {
        let by_id = FdbStore::silo_by_id_key(silo_id);
        if let Ok(Some(bytes)) = store.read_bytes(&by_id).await
            && let Ok(s) = serde_json::from_slice::<Silo>(&bytes)
        {
            // Clean up the default tenant first so the silo's
            // tenant_in_silo index also gets cleared.
            purge_tenant(store, s.default_tenant_id).await;

            let by_name = FdbStore::silo_by_name_key(&s.name);
            let _ = store
                .db
                .run(|tr, _| {
                    let by_id = by_id.clone();
                    let by_name = by_name.clone();
                    async move {
                        tr.clear(&by_id);
                        tr.clear(&by_name);
                        Ok(())
                    }
                })
                .await;
        }
    }

    #[tokio::test]
    #[ignore]
    async fn tenant_round_trip() {
        let store = fdb_test_store();
        let silo = store
            .create_silo(NewSilo {
                name: format!("brand-{}", Uuid::new_v4()),
                description: None,
            })
            .await
            .expect("create silo");

        let t = store
            .create_tenant(
                silo.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: Some("first customer".to_string()),
                },
            )
            .await
            .expect("create tenant");
        assert_eq!(t.silo_id, silo.id);
        assert_eq!(t.name, "acme");
        assert_eq!(t.description, "first customer");

        let fetched = store.get_tenant(t.id).await.expect("get tenant");
        assert_eq!(fetched, t);

        let listed = store
            .list_tenants_in_silo(silo.id)
            .await
            .expect("list tenants");
        assert!(listed.iter().any(|x| x.id == t.id));

        store.delete_tenant(t.id).await.expect("delete tenant");
        let err = store
            .get_tenant(t.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));

        purge_tenant(&store, t.id).await;
        purge_silo(&store, silo.id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn tenants_within_silo_must_have_unique_names() {
        let store = fdb_test_store();
        let silo = store
            .create_silo(NewSilo {
                name: format!("brand-{}", Uuid::new_v4()),
                description: None,
            })
            .await
            .expect("create silo");

        let t = store
            .create_tenant(
                silo.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: None,
                },
            )
            .await
            .expect("create first");
        let err = store
            .create_tenant(
                silo.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("duplicate within silo conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));

        purge_tenant(&store, t.id).await;
        purge_silo(&store, silo.id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn same_tenant_name_in_different_silos_does_not_conflict() {
        let store = fdb_test_store();
        let a = store
            .create_silo(NewSilo {
                name: format!("brand-a-{}", Uuid::new_v4()),
                description: None,
            })
            .await
            .expect("create silo a");
        let b = store
            .create_silo(NewSilo {
                name: format!("brand-b-{}", Uuid::new_v4()),
                description: None,
            })
            .await
            .expect("create silo b");

        let t1 = store
            .create_tenant(
                a.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: None,
                },
            )
            .await
            .expect("create in a");
        let t2 = store
            .create_tenant(
                b.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: None,
                },
            )
            .await
            .expect("same name across silos must be allowed");

        purge_tenant(&store, t1.id).await;
        purge_tenant(&store, t2.id).await;
        purge_silo(&store, a.id).await;
        purge_silo(&store, b.id).await;
    }

    #[tokio::test]
    #[ignore]
    async fn list_tenants_in_unknown_silo_returns_not_found() {
        let store = fdb_test_store();
        let err = store
            .list_tenants_in_silo(Uuid::new_v4())
            .await
            .expect_err("unknown silo should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }
}
