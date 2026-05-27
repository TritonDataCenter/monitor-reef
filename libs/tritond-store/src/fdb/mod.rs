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
//! Every key shape lives in [`keys`]. The parity tests in
//! `keys::tests` pin the on-disk bytes; any drift between this
//! module and the index/lookup expectations breaks them. Writes
//! that touch multiple keys ride a single transaction so name
//! uniqueness and index consistency are enforced atomically.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use foundationdb::options::StreamingMode;
use foundationdb::{Database, FdbBindingError, KeySelector, RangeOption};
use ipnetwork::IpNetwork;
use rand::Rng;
use uuid::Uuid;

use crate::types::{EdgeClusterRecord, NatGatewayRecord};
use crate::validate;
use crate::{
    AddressFamily, ApiKey, AutoApproveWindow, CLAIM_CODE_TTL, Cn, CnCapacity, CnLoadSummary,
    CnPickSnapshot, CnPlacement, CnReservation, CnRole, CnState, DhcpLease, DhcpPool,
    DhcpReservation, Disk, DiskKind, EdgeCluster, EdgeClusterKind, EdgeClusterResource,
    FLOATING_IP_V4_POOL, FLOATING_IP_V6_POOL, FirewallRule, FloatingIp, FloatingIpAttachment,
    IdpConfig, Image, ImageScope, Instance, InstanceAffinity, InstanceBrand, InstanceCreateResult,
    JobOutcome, JobStatus, JobStatusKind, LegacyVm, LifecycleState, LifecycleStateKind, MetaScope,
    MetaValue, MigrationPhase, MigrationProgressEvent, MigrationRecord, MigrationState, NatGateway,
    NetworkResourceId, NewDhcpPool, NewDhcpReservation, NewEdgeCluster, NewFirewallRule,
    NewFloatingIp, NewImage, NewInstance, NewJob, NewMigration, NewNatGateway, NewProject,
    NewQuota, NewRoute, NewRouteTable, NewSilo, NewSshKey, NewStorageCluster, NewSubnet, NewTenant,
    NewVpc, Nic, Project, ProvisioningJob, Quota, Realization, RealizationStatus, RealizerId,
    Route, RouteTable, RouteTarget, Settings, Silo, SshKey, SshKeyScope, StorageCluster,
    StorageClusterStatus, Store, StoreError, Subnet, SystemKey, Tenant, TenantInstanceProjection,
    User, VPC_VNI_MAX, VPC_VNI_RESERVED_CEILING, Vpc, default_boot_disk_size_bytes,
    generate_claim_code, generate_poll_token,
};

/// Maximum attempts to draw a fresh VNI before giving up. Mirrors the
/// in-memory store's cap; with ~16.7M candidates this is operationally
/// unreachable.
mod helpers;
mod keys;

const VNI_RETRY_ATTEMPTS: usize = 8;
const MAIN_ROUTE_TABLE_NAME: &str = "main";

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

// ── Error mapping helpers ─────────────────────────────────────────────
//
// Every `Database::run` closure returns `Result<_, FdbBindingError>`;
// every Store method maps that to `StoreError::Backend(...)`. These
// helpers keep the contextual prefix consistent and let call sites
// stay readable instead of repeating the same `.map_err(|e| ...)`.

impl From<FdbBindingError> for StoreError {
    fn from(e: FdbBindingError) -> Self {
        StoreError::Backend(format!("FDB transaction: {e}"))
    }
}

/// Outside-closure serialize error mapper.
fn ser_err(name: &'static str) -> impl FnOnce(serde_json::Error) -> StoreError {
    move |e| StoreError::Backend(format!("serialize {name}: {e}"))
}

/// Outside-closure deserialize error mapper.
fn de_err(name: &'static str) -> impl FnOnce(serde_json::Error) -> StoreError {
    move |e| StoreError::Backend(format!("deserialize {name}: {e}"))
}

/// Inside-closure serialize error mapper.
fn txn_ser_err(name: &'static str) -> impl FnOnce(serde_json::Error) -> FdbBindingError {
    move |e| FdbBindingError::CustomError(format!("serialize {name}: {e}").into())
}

/// Inside-closure deserialize error mapper.
fn txn_de_err(name: &'static str) -> impl FnOnce(serde_json::Error) -> FdbBindingError {
    move |e| FdbBindingError::CustomError(format!("deserialize {name}: {e}").into())
}

/// Inside-closure general error mapper (UTF-8 conversions, UUID parsing, etc.).
fn txn_err<E: std::fmt::Display>(ctx: &'static str) -> impl FnOnce(E) -> FdbBindingError {
    move |e| FdbBindingError::CustomError(format!("{ctx}: {e}").into())
}

/// Run an FDB transaction. Clones each named capture once per retry
/// iteration (the FDB binding's `db.run` calls the closure `FnMut`, so
/// captures must be owned). Body sees `tr: &Transaction` and returns
/// `Result<T, FdbBindingError>`.
///
/// ```ignore
/// let outcome: Result<Outcome, FdbBindingError> = fdb_txn!(
///     self.db,
///     [by_id_key, by_name_key, value],
///     |tr| {
///         if tr.get(&by_name_key, false).await?.is_some() {
///             return Ok(Outcome::NameTaken);
///         }
///         tr.set(&by_id_key, &value);
///         Ok(Outcome::Created)
///     }
/// );
/// ```
macro_rules! fdb_txn {
    ($db:expr, [$($capture:ident),* $(,)?], |$tr:ident| $body:block) => {{
        $db.run(|$tr, _| {
            $(let $capture = $capture.clone();)*
            async move $body
        }).await
    }};
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

    fn validate_nat_gateway_route_target(
        nat: &NatGatewayRecord,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
    ) -> Result<(), StoreError> {
        if nat.tenant_id != tenant_id || nat.project_id != project_id || nat.vpc_id != vpc_id {
            return Err(StoreError::Conflict(format!(
                "nat gateway target is not in vpc {vpc_id}"
            )));
        }
        Ok(())
    }
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

/// Outcome of `put_idp_config`'s transaction. `IssuerTaken` is the
/// "another tenant already claims this issuer" branch — surfaced
/// to the caller as [`StoreError::Conflict`].
enum PutIdpOutcome {
    Stored,
    IssuerTaken,
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

fn validate_edge_cluster_bound_resource_shape(
    kind: EdgeClusterKind,
    resources: &[EdgeClusterResource],
) -> Result<(), StoreError> {
    let mut seen = HashSet::new();
    for resource in resources {
        if !seen.insert(*resource) {
            return Err(StoreError::Conflict(format!(
                "edge cluster resource {}:{} is listed more than once",
                resource.kind_tag(),
                resource.id()
            )));
        }
        if !kind.accepts_resource(*resource) {
            return Err(StoreError::Conflict(format!(
                "edge cluster kind {kind:?} cannot bind resource {}:{}",
                resource.kind_tag(),
                resource.id()
            )));
        }
    }
    Ok(())
}

fn edge_cluster_resource_record_key(resource: EdgeClusterResource) -> Vec<u8> {
    match resource {
        EdgeClusterResource::NatGateway { nat_gateway_id } => {
            FdbStore::nat_gateway_by_id_key(nat_gateway_id)
        }
        EdgeClusterResource::FloatingIp { floating_ip_id } => {
            FdbStore::floating_ip_by_id_key(floating_ip_id)
        }
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
        validate::name("silo", &req.name)?;
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
            .map_err(ser_err("silo"))?;
        let tenant_value = serde_json::to_vec(&tenant)
            .map_err(ser_err("tenant"))?;
        let silo_by_id_key = keys::silo_by_id_key(silo.id);
        let silo_by_name_key = keys::silo_by_name_key(&silo.name);
        let tenant_by_id_key = keys::tenant_by_id_key(tenant.id);
        let tenant_by_name_key = keys::tenant_by_silo_name_key(silo_id, &tenant.name);
        let tenant_in_silo_key = keys::tenant_in_silo_key(silo_id, tenant.id);
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
            Err(e) => Err(e.into()),
        }
    }

    async fn get_silo(&self, id: Uuid) -> Result<Silo, StoreError> {
        let key = keys::silo_by_id_key(id);
        let bytes = self.read_bytes(&key).await?;
        match bytes {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(de_err("silo")),
            None => Err(StoreError::NotFound),
        }
    }

    async fn create_user(&self, user: User) -> Result<User, StoreError> {
        validate::name("username", &user.username)?;
        let value = serde_json::to_vec(&user)
            .map_err(ser_err("user"))?;
        let by_id_key = keys::user_by_id_key(user.id);
        let by_name_key = keys::user_by_name_key(&user.username);
        // Federation index is keyed by (tenant_id, issuer, subject) —
        // post E-5 the IdP is tenant-scoped, so the index is rooted
        // directly at the tenant. The defensive tenant existence
        // check still happens (a federated user without a tenant is
        // a programming error).
        let federation_key = match (user.tenant_id, user.federation.as_ref()) {
            (Some(tenant_id), Some(fed)) => {
                // Confirm the tenant exists; fail clean otherwise.
                let _ = self.get_tenant(tenant_id).await?;
                Some(keys::user_federation_key(
                    tenant_id,
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
            Err(e) => Err(e.into()),
        }
    }

    async fn get_user_by_username(&self, username: &str) -> Result<User, StoreError> {
        let by_name_key = keys::user_by_name_key(username);
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
        let key = keys::user_by_id_key(id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("user"))
    }

    async fn update_user_password_hash(
        &self,
        username: &str,
        password_hash: String,
    ) -> Result<User, StoreError> {
        let by_name_key = keys::user_by_name_key(username);
        let username = username.to_string();
        let result: Result<Option<User>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_name_key = by_name_key.clone();
                let password_hash = password_hash.clone();
                let username = username.clone();
                async move {
                    let Some(id_bytes) = tr.get(&by_name_key, false).await? else {
                        return Ok(None);
                    };
                    let id_str = std::str::from_utf8(id_bytes.as_ref()).map_err(txn_err("user index value not utf8"))?;
                    let id = Uuid::parse_str(id_str).map_err(txn_err("user index value not uuid"))?;
                    let by_id_key = keys::user_by_id_key(id);
                    let Some(user_bytes) = tr.get(&by_id_key, false).await? else {
                        return Ok(None);
                    };
                    let mut user: User =
                        serde_json::from_slice(user_bytes.as_ref()).map_err(txn_de_err("user"))?;
                    if user.username != username {
                        return Ok(None);
                    }
                    user.password_hash = password_hash;
                    let value = serde_json::to_vec(&user).map_err(txn_ser_err("user"))?;
                    tr.set(&by_id_key, &value);
                    Ok(Some(user))
                }
            })
            .await;

        match result {
            Ok(Some(user)) => Ok(user),
            Ok(None) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn migrate_user_capabilities(&self) -> Result<usize, StoreError> {
        use crate::Capability;
        // One-shot scan + per-row rewrite. Idempotent: only rewrites
        // when the persisted row has an empty capability set, so a
        // second call is a no-op. Each rewrite is its own
        // single-key transaction; not batched into a 5.0MB FDB
        // transaction because the User keyspace is small and the
        // migration runs once at bootstrap.
        let (begin, end) = prefix_range(keys::user_prefix());
        let users: Result<Vec<User>, FdbBindingError> = self
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
                    let mut out = Vec::new();
                    for kv in kvs.iter() {
                        if let Ok(u) = serde_json::from_slice::<User>(kv.value()) {
                            out.push(u);
                        }
                    }
                    Ok(out)
                }
            })
            .await;
        let users = users.map_err(StoreError::from)?;

        let mut rewritten = 0usize;
        for user in users {
            if !user.capabilities.is_empty() {
                continue;
            }
            let new_caps: std::collections::BTreeSet<Capability> = if user.is_root {
                Capability::all().iter().copied().collect()
            } else if user.fleet_admin {
                let mut s = std::collections::BTreeSet::new();
                s.insert(Capability::SystemRead);
                s.insert(Capability::SystemOperate);
                s
            } else {
                continue;
            };
            let mut updated = user.clone();
            updated.capabilities = new_caps;
            let by_id_key = keys::user_by_id_key(updated.id);
            let value = serde_json::to_vec(&updated)
                .map_err(ser_err("user"))?;
            let result: Result<(), FdbBindingError> = self
                .db
                .run(|tr, _| {
                    let by_id_key = by_id_key.clone();
                    let value = value.clone();
                    async move {
                        tr.set(&by_id_key, &value);
                        Ok(())
                    }
                })
                .await;
            result.map_err(StoreError::from)?;
            rewritten += 1;
        }
        Ok(rewritten)
    }

    async fn update_user_capabilities(
        &self,
        user_id: Uuid,
        capabilities: std::collections::BTreeSet<crate::Capability>,
    ) -> Result<User, StoreError> {
        let by_id_key = keys::user_by_id_key(user_id);
        enum Out {
            Updated(Box<User>),
            Vanished,
        }
        let outcome: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let capabilities = capabilities.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Out::Vanished),
                    };
                    let mut user: User = match serde_json::from_slice(&bytes) {
                        Ok(u) => u,
                        Err(_) => return Ok(Out::Vanished),
                    };
                    user.capabilities = capabilities;
                    let value = match serde_json::to_vec(&user) {
                        Ok(v) => v,
                        Err(_) => return Ok(Out::Vanished),
                    };
                    tr.set(&by_id_key, &value);
                    Ok(Out::Updated(Box::new(user)))
                }
            })
            .await;
        match outcome {
            Ok(Out::Updated(u)) => Ok(*u),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn has_any_user(&self) -> Result<bool, StoreError> {
        let (begin, end) = prefix_range(keys::user_prefix());
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
        result.map_err(StoreError::from)
    }

    async fn create_api_key(&self, key: ApiKey) -> Result<ApiKey, StoreError> {
        let value = serde_json::to_vec(&key)
            .map_err(ser_err("api key"))?;
        let by_id_key = keys::apikey_by_id_key(key.id);
        let by_lookup_key = keys::apikey_by_lookup_key(&key.lookup_id);
        let user_index_key = keys::apikey_user_index_key(key.user_id, key.id);
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
            Err(e) => Err(e.into()),
        }
    }

    async fn list_api_keys(&self, user_id: Uuid) -> Result<Vec<ApiKey>, StoreError> {
        let prefix = keys::apikey_user_index_prefix(user_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        // Collect the key ids that this user owns.
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("api key index uuid: {e}")))?;
            let by_id_key = keys::apikey_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let key: ApiKey = serde_json::from_slice(&bytes)
                    .map_err(de_err("api key"))?;
                out.push(key);
            }
        }
        Ok(out)
    }

    async fn get_api_key_by_lookup_id(&self, lookup_id: &str) -> Result<ApiKey, StoreError> {
        let by_lookup_key = keys::apikey_by_lookup_key(lookup_id);
        let id_bytes = self
            .read_bytes(&by_lookup_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("api key lookup index not utf8: {e}")))?;
        let id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("api key lookup index not uuid: {e}")))?;
        let by_id_key = keys::apikey_by_id_key(id);
        let bytes = self
            .read_bytes(&by_id_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("api key"))
    }

    async fn delete_api_key(&self, user_id: Uuid, key_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::apikey_by_id_key(key_id);
        let user_index_key = keys::apikey_user_index_key(user_id, key_id);

        let outcome: Result<DeleteOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let user_index_key = user_index_key.clone();
                async move {
                    // user-index entry is the source of truth for
                    // ownership; if it's gone, somebody already
                    // deleted the key.
                    if tr.get(&user_index_key, false).await?.is_none() {
                        return Ok(DeleteOutcome::NotFound);
                    }
                    // Read the record inside the txn so the lookup_id
                    // we clear is consistent with the commit snapshot.
                    let record_bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(DeleteOutcome::NotFound),
                    };
                    let record: ApiKey =
                        serde_json::from_slice(&record_bytes).map_err(txn_de_err("api key"))?;
                    if record.user_id != user_id {
                        return Ok(DeleteOutcome::NotFound);
                    }
                    let by_lookup_key = keys::apikey_by_lookup_key(&record.lookup_id);
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
            Err(e) => Err(e.into()),
        }
    }

    async fn get_settings(&self) -> Result<Settings, StoreError> {
        let key = keys::settings_key().to_vec();
        match self.read_bytes(&key).await? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(de_err("settings")),
            None => Ok(Settings::default()),
        }
    }

    async fn put_settings(&self, settings: Settings) -> Result<(), StoreError> {
        let value = serde_json::to_vec(&settings)
            .map_err(ser_err("settings"))?;
        let key = keys::settings_key().to_vec();
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
        result.map_err(StoreError::from)
    }

    async fn get_system_key(&self, key: SystemKey) -> Result<Vec<u8>, StoreError> {
        let storage_key = keys::system_key(key);
        self.read_bytes(&storage_key)
            .await?
            .ok_or(StoreError::NotFound)
    }

    async fn put_system_key(&self, key: SystemKey, value: Vec<u8>) -> Result<(), StoreError> {
        let storage_key = keys::system_key(key);
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
        result.map_err(StoreError::from)
    }

    // ---- Layered instance metadata (IMDS) ----
    //
    // `meta/<scope>/<uuid>/<key>` -> JSON-encoded MetaValue;
    // `meta/gen/<scope>/<uuid>` -> big-endian u64 generation (absent ==
    // 0), bumped in the same transaction as every write/delete so the
    // realized-view cache can key off the four-scope gen tuple.

    async fn set_meta(
        &self,
        scope: MetaScope,
        scope_id: Uuid,
        key: &str,
        value: MetaValue,
    ) -> Result<u64, StoreError> {
        crate::types::validate_meta_key(scope, key)
            .map_err(|e| StoreError::Conflict(e.to_string()))?;
        let entry_key = keys::meta_entry_key(scope, scope_id, key);
        let gen_key = keys::meta_gen_key(scope, scope_id);
        let encoded = serde_json::to_vec(&value)
            .map_err(ser_err("MetaValue"))?;
        let result: Result<u64, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let entry_key = entry_key.clone();
                let gen_key = gen_key.clone();
                let encoded = encoded.clone();
                async move {
                    tr.set(&entry_key, &encoded);
                    let cur = tr
                        .get(&gen_key, false)
                        .await?
                        .and_then(|s| <[u8; 8]>::try_from(s.as_ref()).ok())
                        .map(u64::from_be_bytes)
                        .unwrap_or(0);
                    let next = cur.saturating_add(1);
                    tr.set(&gen_key, &next.to_be_bytes());
                    Ok(next)
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    async fn get_meta(
        &self,
        scope: MetaScope,
        scope_id: Uuid,
        key: &str,
    ) -> Result<MetaValue, StoreError> {
        let entry_key = keys::meta_entry_key(scope, scope_id, key);
        match self.read_bytes(&entry_key).await? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(de_err("MetaValue")),
            None => Err(StoreError::NotFound),
        }
    }

    async fn delete_meta(
        &self,
        scope: MetaScope,
        scope_id: Uuid,
        key: &str,
    ) -> Result<u64, StoreError> {
        let entry_key = keys::meta_entry_key(scope, scope_id, key);
        let gen_key = keys::meta_gen_key(scope, scope_id);
        let result: Result<Option<u64>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let entry_key = entry_key.clone();
                let gen_key = gen_key.clone();
                async move {
                    if tr.get(&entry_key, false).await?.is_none() {
                        return Ok(None);
                    }
                    tr.clear(&entry_key);
                    let cur = tr
                        .get(&gen_key, false)
                        .await?
                        .and_then(|s| <[u8; 8]>::try_from(s.as_ref()).ok())
                        .map(u64::from_be_bytes)
                        .unwrap_or(0);
                    let next = cur.saturating_add(1);
                    tr.set(&gen_key, &next.to_be_bytes());
                    Ok(Some(next))
                }
            })
            .await;
        match result.map_err(StoreError::from)? {
            Some(next) => Ok(next),
            None => Err(StoreError::NotFound),
        }
    }

    async fn list_meta(
        &self,
        scope: MetaScope,
        scope_id: Uuid,
    ) -> Result<Vec<(String, MetaValue)>, StoreError> {
        let prefix = keys::meta_scope_prefix(scope, scope_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();
        let raw: Result<Vec<(String, Vec<u8>)>, FdbBindingError> = self
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
                    let mut out = Vec::new();
                    for kv in kvs.iter() {
                        if kv.key().len() <= prefix_len {
                            continue;
                        }
                        let key_bytes = &kv.key()[prefix_len..];
                        if let Ok(k) = std::str::from_utf8(key_bytes) {
                            out.push((k.to_string(), kv.value().to_vec()));
                        }
                    }
                    Ok(out)
                }
            })
            .await;
        let raw = raw.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(raw.len());
        for (k, bytes) in raw {
            let v: MetaValue = serde_json::from_slice(&bytes)
                .map_err(de_err("MetaValue"))?;
            out.push((k, v));
        }
        Ok(out)
    }

    async fn get_meta_gen(&self, scope: MetaScope, scope_id: Uuid) -> Result<u64, StoreError> {
        let gen_key = keys::meta_gen_key(scope, scope_id);
        Ok(self
            .read_bytes(&gen_key)
            .await?
            .and_then(|s| <[u8; 8]>::try_from(s.as_slice()).ok())
            .map(u64::from_be_bytes)
            .unwrap_or(0))
    }

    async fn get_user_by_federation(
        &self,
        tenant_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<User, StoreError> {
        let federation_key = keys::user_federation_key(tenant_id, issuer, subject);
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
        tenant_id: Uuid,
        config: IdpConfig,
    ) -> Result<IdpConfig, StoreError> {
        let by_tenant_key = keys::idp_config_key(tenant_id);
        let by_issuer_key = keys::idp_by_issuer_key(&config.issuer_url);
        let value = serde_json::to_vec(&config)
            .map_err(ser_err("idp config"))?;
        let tenant_id_str = tenant_id.to_string();

        // Single transaction:
        //   1. Read by_issuer/<hash>. If present and points to a
        //      different tenant, return Conflict.
        //   2. Read by_tenant/<tenant>. If present, derive the old
        //      issuer's by_issuer key and clear it (we may be
        //      changing this tenant's issuer).
        //   3. Write by_tenant/<tenant> = JSON config.
        //   4. Write by_issuer/<hash> = tenant_id bytes.
        let outcome: Result<PutIdpOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_tenant_key = by_tenant_key.clone();
                let by_issuer_key = by_issuer_key.clone();
                let value = value.clone();
                let tenant_id_bytes = tenant_id_str.as_bytes().to_vec();
                async move {
                    if let Some(claimed) = tr.get(&by_issuer_key, false).await?
                        && claimed.as_ref() != tenant_id_bytes.as_slice()
                    {
                        return Ok(PutIdpOutcome::IssuerTaken);
                    }
                    if let Some(prev_bytes) = tr.get(&by_tenant_key, false).await? {
                        let prev: IdpConfig = serde_json::from_slice(&prev_bytes).map_err(txn_de_err("prev idp config"))?;
                        // Drop the stale issuer→tenant entry when
                        // the tenant is moving to a different issuer.
                        let prev_issuer_key = FdbStore::idp_by_issuer_key(&prev.issuer_url);
                        if prev_issuer_key != by_issuer_key {
                            tr.clear(&prev_issuer_key);
                        }
                    }
                    tr.set(&by_tenant_key, &value);
                    tr.set(&by_issuer_key, &tenant_id_bytes);
                    Ok(PutIdpOutcome::Stored)
                }
            })
            .await;
        match outcome {
            Ok(PutIdpOutcome::Stored) => Ok(config),
            Ok(PutIdpOutcome::IssuerTaken) => Err(StoreError::Conflict(format!(
                "issuer {:?} already claimed by another tenant",
                config.issuer_url
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_idp_config(&self, tenant_id: Uuid) -> Result<IdpConfig, StoreError> {
        let key = keys::idp_config_key(tenant_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("idp config"))
    }

    async fn delete_idp_config(&self, tenant_id: Uuid) -> Result<(), StoreError> {
        let by_tenant_key = keys::idp_config_key(tenant_id);
        let outcome: Result<DeleteOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_tenant_key = by_tenant_key.clone();
                async move {
                    let Some(prev_bytes) = tr.get(&by_tenant_key, false).await? else {
                        return Ok(DeleteOutcome::NotFound);
                    };
                    // Best-effort clear of the matching by_issuer
                    // entry. If the JSON deserialise fails we still
                    // clear the by_tenant key — leaving a stale
                    // by_issuer entry behind is preferable to
                    // refusing to delete a corrupt record.
                    if let Ok(prev) = serde_json::from_slice::<IdpConfig>(&prev_bytes) {
                        let by_issuer_key = FdbStore::idp_by_issuer_key(&prev.issuer_url);
                        tr.clear(&by_issuer_key);
                    }
                    tr.clear(&by_tenant_key);
                    Ok(DeleteOutcome::Deleted)
                }
            })
            .await;
        match outcome {
            Ok(DeleteOutcome::Deleted) => Ok(()),
            Ok(DeleteOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_idp_configs(&self) -> Result<Vec<(Uuid, IdpConfig)>, StoreError> {
        let (begin, end) = prefix_range(keys::idp_config_prefix());
        let prefix_len = keys::idp_config_prefix().len();

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
        let raws = result.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(raws.len());
        for (key, value) in raws {
            let suffix = &key[prefix_len..];
            let tenant_str = std::str::from_utf8(suffix)
                .map_err(|e| StoreError::Backend(format!("idp index key not utf8: {e}")))?;
            let tenant_id = Uuid::parse_str(tenant_str)
                .map_err(|e| StoreError::Backend(format!("idp index key not uuid: {e}")))?;
            let config: IdpConfig = serde_json::from_slice(&value)
                .map_err(de_err("idp config"))?;
            out.push((tenant_id, config));
        }
        Ok(out)
    }

    async fn get_idp_config_by_issuer(
        &self,
        issuer: &str,
    ) -> Result<(Uuid, IdpConfig), StoreError> {
        let by_issuer_key = keys::idp_by_issuer_key(issuer);
        let id_bytes = self
            .read_bytes(&by_issuer_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("idp issuer index value not utf8: {e}")))?;
        let tenant_id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("idp issuer index value not uuid: {e}")))?;
        let config = self.get_idp_config(tenant_id).await?;
        Ok((tenant_id, config))
    }

    async fn create_project(
        &self,
        tenant_id: Uuid,
        req: NewProject,
    ) -> Result<Project, StoreError> {
        validate::name("project", &req.name)?;
        let project = Project {
            id: Uuid::new_v4(),
            tenant_id,
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&project)
            .map_err(ser_err("project"))?;
        let by_id_key = keys::project_by_id_key(project.id);
        let by_name_key = keys::project_by_tenant_name_key(tenant_id, &project.name);
        let in_tenant_key = keys::project_in_tenant_key(tenant_id, project.id);
        let tenant_check_key = keys::tenant_by_id_key(tenant_id);
        let id_str = project.id.to_string();
        let name_str = project.name.clone();

        // Outcome distinguishes tenant-missing from name-conflict so the
        // single transaction can convey both into our caller's error
        // shape.
        enum Outcome {
            Created,
            TenantMissing,
            NameTaken,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_tenant_key = in_tenant_key.clone();
                let tenant_check_key = tenant_check_key.clone();
                let value = value.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&tenant_check_key, false).await?.is_none() {
                        return Ok(Outcome::TenantMissing);
                    }
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_tenant_key, b"");
                    Ok(Outcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created) => Ok(project),
            Ok(Outcome::TenantMissing) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "project with name {name_str:?} already exists in tenant {tenant_id}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_project(&self, project_id: Uuid) -> Result<Project, StoreError> {
        let key = keys::project_by_id_key(project_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("project"))
    }

    async fn list_projects_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Project>, StoreError> {
        let prefix = keys::project_in_tenant_prefix(tenant_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("project index uuid: {e}")))?;
            let by_id_key = keys::project_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let project: Project = serde_json::from_slice(&bytes)
                    .map_err(de_err("project"))?;
                out.push(project);
            }
        }
        Ok(out)
    }

    async fn delete_project(&self, project_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::project_by_id_key(project_id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(DelOut::Vanished),
                    };
                    let project: Project =
                        serde_json::from_slice(&bytes).map_err(txn_de_err("project"))?;
                    let by_name_key =
                        keys::project_by_tenant_name_key(project.tenant_id, &project.name);
                    let in_tenant_key =
                        keys::project_in_tenant_key(project.tenant_id, project.id);
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_tenant_key);
                    Ok(DelOut::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DelOut::Deleted) => Ok(()),
            Ok(DelOut::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn create_tenant(&self, silo_id: Uuid, req: NewTenant) -> Result<Tenant, StoreError> {
        validate::name("tenant", &req.name)?;
        let tenant = Tenant {
            id: Uuid::new_v4(),
            silo_id,
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&tenant)
            .map_err(ser_err("tenant"))?;
        let by_id_key = keys::tenant_by_id_key(tenant.id);
        let by_name_key = keys::tenant_by_silo_name_key(silo_id, &tenant.name);
        let in_silo_key = keys::tenant_in_silo_key(silo_id, tenant.id);
        let silo_check_key = keys::silo_by_id_key(silo_id);
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
            Err(e) => Err(e.into()),
        }
    }

    async fn get_tenant(&self, tenant_id: Uuid) -> Result<Tenant, StoreError> {
        let key = keys::tenant_by_id_key(tenant_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("tenant"))
    }

    async fn list_tenants_in_silo(&self, silo_id: Uuid) -> Result<Vec<Tenant>, StoreError> {
        // Confirm the silo exists first so callers can distinguish
        // "silo missing" (NotFound) from "silo present but empty"
        // (empty Vec).
        let silo_check_key = keys::silo_by_id_key(silo_id);
        if self.read_bytes(&silo_check_key).await?.is_none() {
            return Err(StoreError::NotFound);
        }

        let prefix = keys::tenant_in_silo_prefix(silo_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("tenant index uuid: {e}")))?;
            let by_id_key = keys::tenant_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let tenant: Tenant = serde_json::from_slice(&bytes)
                    .map_err(de_err("tenant"))?;
                out.push(tenant);
            }
        }
        Ok(out)
    }

    async fn delete_tenant(&self, tenant_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::tenant_by_id_key(tenant_id);

        // TODO(slice E-3): reject deletion when child projects exist
        let outcome: Result<DeleteOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(DeleteOutcome::NotFound),
                    };
                    let tenant: Tenant =
                        serde_json::from_slice(&bytes).map_err(txn_de_err("tenant"))?;
                    let by_name_key =
                        keys::tenant_by_silo_name_key(tenant.silo_id, &tenant.name);
                    let in_silo_key = keys::tenant_in_silo_key(tenant.silo_id, tenant.id);
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
            Err(e) => Err(e.into()),
        }
    }

    async fn create_vpc(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewVpc,
    ) -> Result<Vpc, StoreError> {
        validate::name("vpc", &req.name)?;
        // Outcome distinguishes our four invariant failures from FDB
        // transport errors. VniTaken triggers a retry at this layer
        // (a fresh draw + new transaction); the others surface to the
        // caller verbatim.
        enum Outcome {
            Created(Vpc),
            ProjectMissingOrWrongTenant,
            NameTaken,
            VniTaken,
        }

        let project_check_key = keys::project_by_id_key(project_id);
        let by_name_key = keys::vpc_by_project_name_key(project_id, &req.name);

        for _ in 0..VNI_RETRY_ATTEMPTS {
            let vni = rand::rng().random_range(VPC_VNI_RESERVED_CEILING..VPC_VNI_MAX);
            let vpc_id = Uuid::new_v4();
            let route_table_id = Uuid::new_v4();
            let now = Utc::now();
            let main_route_table = RouteTable {
                id: route_table_id,
                tenant_id,
                project_id,
                vpc_id,
                name: MAIN_ROUTE_TABLE_NAME.to_string(),
                description: format!("Main route table for VPC {}", req.name),
                is_main: true,
                created_at: now,
            };
            let candidate = Vpc {
                id: vpc_id,
                tenant_id,
                project_id,
                main_route_table_id: route_table_id,
                name: req.name.clone(),
                description: req.description.clone().unwrap_or_default(),
                vni,
                ipv4_block: req.ipv4_block,
                ipv6_block: req.ipv6_block,
                created_at: now,
            };
            let value = serde_json::to_vec(&candidate)
                .map_err(ser_err("vpc"))?;
            let main_route_table_value = serde_json::to_vec(&main_route_table)
                .map_err(ser_err("route table"))?;
            let by_id_key = keys::vpc_by_id_key(candidate.id);
            let in_project_key = keys::vpc_in_project_key(project_id, candidate.id);
            let by_vni_key = keys::vpc_by_vni_key(vni);
            let rt_by_id_key = keys::route_table_by_id_key(route_table_id);
            let rt_by_name_key = keys::route_table_by_vpc_name_key(vpc_id, MAIN_ROUTE_TABLE_NAME);
            let rt_in_vpc_key = keys::route_table_in_vpc_key(vpc_id, route_table_id);
            let rt_main_key = keys::route_table_main_key(vpc_id);
            let id_str = candidate.id.to_string();
            let route_table_id_str = route_table_id.to_string();

            let outcome: Result<Outcome, FdbBindingError> = self
                .db
                .run(|tr, _| {
                    let project_check_key = project_check_key.clone();
                    let by_id_key = by_id_key.clone();
                    let by_name_key = by_name_key.clone();
                    let in_project_key = in_project_key.clone();
                    let by_vni_key = by_vni_key.clone();
                    let rt_by_id_key = rt_by_id_key.clone();
                    let rt_by_name_key = rt_by_name_key.clone();
                    let rt_in_vpc_key = rt_in_vpc_key.clone();
                    let rt_main_key = rt_main_key.clone();
                    let value = value.clone();
                    let main_route_table_value = main_route_table_value.clone();
                    let id_bytes = id_str.as_bytes().to_vec();
                    let route_table_id_bytes = route_table_id_str.as_bytes().to_vec();
                    let candidate = candidate.clone();
                    async move {
                        // Project must exist and live in the tenant the
                        // caller claims. Tenant mismatch surfaces as
                        // NotFound (project is invisible to a foreign
                        // tenant).
                        let project_bytes = match tr.get(&project_check_key, false).await? {
                            Some(b) => b,
                            None => return Ok(Outcome::ProjectMissingOrWrongTenant),
                        };
                        let project: Project = match serde_json::from_slice(&project_bytes) {
                            Ok(p) => p,
                            Err(_) => return Ok(Outcome::ProjectMissingOrWrongTenant),
                        };
                        if project.tenant_id != tenant_id {
                            return Ok(Outcome::ProjectMissingOrWrongTenant);
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
                        tr.set(&rt_by_id_key, &main_route_table_value);
                        tr.set(&rt_by_name_key, &route_table_id_bytes);
                        tr.set(&rt_in_vpc_key, b"");
                        tr.set(&rt_main_key, &route_table_id_bytes);
                        Ok(Outcome::Created(candidate))
                    }
                })
                .await;

            match outcome {
                Ok(Outcome::Created(vpc)) => return Ok(vpc),
                Ok(Outcome::ProjectMissingOrWrongTenant) => return Err(StoreError::NotFound),
                Ok(Outcome::NameTaken) => {
                    return Err(StoreError::Conflict(format!(
                        "vpc with name {:?} already exists in project {project_id}",
                        req.name
                    )));
                }
                Ok(Outcome::VniTaken) => continue,
                Err(e) => return Err(e.into()),
            }
        }

        Err(StoreError::Backend(format!(
            "VNI exhausted after {VNI_RETRY_ATTEMPTS} retries"
        )))
    }

    async fn get_vpc(&self, vpc_id: Uuid) -> Result<Vpc, StoreError> {
        let key = keys::vpc_by_id_key(vpc_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("vpc"))
    }

    async fn list_vpcs_in_project(&self, project_id: Uuid) -> Result<Vec<Vpc>, StoreError> {
        let prefix = keys::vpc_in_project_prefix(project_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("vpc index uuid: {e}")))?;
            let by_id_key = keys::vpc_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let vpc: Vpc = serde_json::from_slice(&bytes)
                    .map_err(de_err("vpc"))?;
                out.push(vpc);
            }
        }
        Ok(out)
    }

    async fn delete_vpc(&self, vpc_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::vpc_by_id_key(vpc_id);
        let subnet_prefix = keys::subnet_in_vpc_prefix(vpc_id);
        let (subnet_begin, subnet_end) = prefix_range(&subnet_prefix);
        let route_table_prefix = keys::route_table_in_vpc_prefix(vpc_id);
        let (route_table_begin, route_table_end) = prefix_range(&route_table_prefix);
        let route_table_prefix_len = route_table_prefix.len();
        let main_rt_by_name_key = keys::route_table_by_vpc_name_key(vpc_id, MAIN_ROUTE_TABLE_NAME);
        let main_rt_singleton_key = keys::route_table_main_key(vpc_id);

        enum DelOut {
            Deleted,
            Vanished,
            HasSubnets,
            HasRouteTables,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let subnet_begin = subnet_begin.clone();
                let subnet_end = subnet_end.clone();
                let route_table_begin = route_table_begin.clone();
                let route_table_end = route_table_end.clone();
                let main_rt_by_name_key = main_rt_by_name_key.clone();
                let main_rt_singleton_key = main_rt_singleton_key.clone();
                async move {
                    // Read inside the txn so index clears below come from a
                    // row consistent with the commit snapshot. (Reading
                    // outside would let a concurrent update slip in between.)
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(DelOut::Vanished),
                    };
                    let vpc: Vpc = serde_json::from_slice(&bytes).map_err(txn_de_err("vpc"))?;
                    let by_name_key =
                        keys::vpc_by_project_name_key(vpc.project_id, &vpc.name);
                    let in_project_key = keys::vpc_in_project_key(vpc.project_id, vpc.id);
                    let by_vni_key = keys::vpc_by_vni_key(vpc.vni);
                    let main_rt_by_id_key = keys::route_table_by_id_key(vpc.main_route_table_id);
                    let main_rt_in_vpc_key =
                        keys::route_table_in_vpc_key(vpc_id, vpc.main_route_table_id);

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
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(route_table_begin),
                        end: KeySelector::first_greater_or_equal(route_table_end),
                        limit: Some(2),
                        ..RangeOption::default()
                    };
                    let route_table_kvs = tr.get_range(&opt, 1, false).await?;
                    for kv in route_table_kvs.iter() {
                        let suffix = &kv.key()[route_table_prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix)
                            && let Ok(id) = Uuid::parse_str(s)
                            && id != vpc.main_route_table_id
                        {
                            return Ok(DelOut::HasRouteTables);
                        }
                    }
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_project_key);
                    tr.clear(&by_vni_key);
                    tr.clear(&main_rt_by_id_key);
                    tr.clear(&main_rt_by_name_key);
                    tr.clear(&main_rt_in_vpc_key);
                    tr.clear(&main_rt_singleton_key);
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
            Ok(DelOut::HasRouteTables) => Err(StoreError::Conflict(format!(
                "vpc {vpc_id} still has route tables attached; delete route tables first"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn create_subnet(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewSubnet,
    ) -> Result<Subnet, StoreError> {
        validate::name("subnet", &req.name)?;
        let vpc_check_key = keys::vpc_by_id_key(vpc_id);
        let subnet_prefix = keys::subnet_in_vpc_prefix(vpc_id);
        let (peer_begin, peer_end) = prefix_range(&subnet_prefix);

        // The candidate is finalized inside the transaction so
        // `created_at` and the new uuid are stable across the run.
        let candidate_id = Uuid::new_v4();
        let by_id_key = keys::subnet_by_id_key(candidate_id);
        let by_name_key = keys::subnet_by_vpc_name_key(vpc_id, &req.name);
        let in_vpc_key = keys::subnet_in_vpc_key(vpc_id, candidate_id);
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
                    if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
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
                        let peer_key = keys::subnet_by_id_key(peer_id);
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
                        tenant_id,
                        project_id,
                        vpc_id,
                        route_table_id: vpc.main_route_table_id,
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
            Err(e) => Err(e.into()),
        }
    }

    async fn get_subnet(&self, subnet_id: Uuid) -> Result<Subnet, StoreError> {
        let key = keys::subnet_by_id_key(subnet_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("subnet"))
    }

    async fn list_subnets_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<Subnet>, StoreError> {
        let prefix = keys::subnet_in_vpc_prefix(vpc_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("subnet index uuid: {e}")))?;
            let by_id_key = keys::subnet_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let subnet: Subnet = serde_json::from_slice(&bytes)
                    .map_err(de_err("subnet"))?;
                out.push(subnet);
            }
        }
        Ok(out)
    }

    async fn delete_subnet(&self, subnet_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::subnet_by_id_key(subnet_id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(DelOut::Vanished),
                    };
                    let subnet: Subnet =
                        serde_json::from_slice(&bytes).map_err(txn_de_err("subnet"))?;
                    let by_name_key = keys::subnet_by_vpc_name_key(subnet.vpc_id, &subnet.name);
                    let in_vpc_key = keys::subnet_in_vpc_key(subnet.vpc_id, subnet.id);
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
            Err(e) => Err(e.into()),
        }
    }

    async fn create_route_table(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewRouteTable,
    ) -> Result<RouteTable, StoreError> {
        validate::name("route_table", &req.name)?;
        let vpc_check_key = keys::vpc_by_id_key(vpc_id);
        let by_name_key = keys::route_table_by_vpc_name_key(vpc_id, &req.name);
        let route_table_id = Uuid::new_v4();
        let by_id_key = keys::route_table_by_id_key(route_table_id);
        let in_vpc_key = keys::route_table_in_vpc_key(vpc_id, route_table_id);
        let id_str = route_table_id.to_string();

        enum Outcome {
            Created(RouteTable),
            VpcMissingOrWrongParent,
            NameTaken,
            SerializeFailed(String),
        }

        let req_for_txn = req.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let vpc_check_key = vpc_check_key.clone();
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_vpc_key = in_vpc_key.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                let req = req_for_txn.clone();
                async move {
                    let vpc_bytes = match tr.get(&vpc_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::VpcMissingOrWrongParent),
                    };
                    let vpc: Vpc = match serde_json::from_slice(&vpc_bytes) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::VpcMissingOrWrongParent),
                    };
                    if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
                        return Ok(Outcome::VpcMissingOrWrongParent);
                    }
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }

                    let route_table = RouteTable {
                        id: route_table_id,
                        tenant_id,
                        project_id,
                        vpc_id,
                        name: req.name.clone(),
                        description: req.description.unwrap_or_default(),
                        is_main: false,
                        created_at: Utc::now(),
                    };
                    let value = match serde_json::to_vec(&route_table) {
                        Ok(v) => v,
                        Err(e) => return Ok(Outcome::SerializeFailed(e.to_string())),
                    };
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_vpc_key, b"");
                    Ok(Outcome::Created(route_table))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(route_table)) => Ok(route_table),
            Ok(Outcome::VpcMissingOrWrongParent) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "route table with name {:?} already exists in vpc {vpc_id}",
                req.name
            ))),
            Ok(Outcome::SerializeFailed(e)) => {
                Err(StoreError::Backend(format!("serialize route table: {e}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn get_route_table(&self, route_table_id: Uuid) -> Result<RouteTable, StoreError> {
        let key = keys::route_table_by_id_key(route_table_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("route table"))
    }

    async fn list_route_tables_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<RouteTable>, StoreError> {
        let prefix = keys::route_table_in_vpc_prefix(vpc_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("route table index uuid: {e}")))?;
            match self.get_route_table(id).await {
                Ok(route_table) => out.push(route_table),
                Err(StoreError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn delete_route_table(&self, route_table_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::route_table_by_id_key(route_table_id);

        enum Out {
            Deleted,
            Vanished,
            Main,
            HasRoutes,
            HasSubnetAssociations,
            Corrupt(String),
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
                    let route_table: RouteTable = match serde_json::from_slice(&bytes) {
                        Ok(rt) => rt,
                        Err(e) => return Ok(Out::Corrupt(e.to_string())),
                    };
                    if route_table.is_main {
                        return Ok(Out::Main);
                    }

                    let route_prefix = keys::route_in_table_prefix(route_table_id);
                    let (route_begin, route_end) = prefix_range(&route_prefix);
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(route_begin),
                        end: KeySelector::first_greater_or_equal(route_end),
                        limit: Some(1),
                        ..RangeOption::default()
                    };
                    let route_kvs = tr.get_range(&opt, 1, false).await?;
                    if route_kvs.iter().next().is_some() {
                        return Ok(Out::HasRoutes);
                    }
                    drop(route_kvs);

                    let subnet_prefix = keys::subnet_in_vpc_prefix(route_table.vpc_id);
                    let (subnet_begin, subnet_end) = prefix_range(&subnet_prefix);
                    let prefix_len = subnet_prefix.len();
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(subnet_begin),
                        end: KeySelector::first_greater_or_equal(subnet_end),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    let mut subnet_ids = Vec::new();
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        if let Ok(s) = std::str::from_utf8(suffix)
                            && let Ok(id) = Uuid::parse_str(s)
                        {
                            subnet_ids.push(id);
                        }
                    }
                    drop(kvs);
                    for subnet_id in subnet_ids {
                        let subnet_key = keys::subnet_by_id_key(subnet_id);
                        let Some(subnet_bytes) = tr.get(&subnet_key, false).await? else {
                            continue;
                        };
                        let Ok(subnet) = serde_json::from_slice::<Subnet>(&subnet_bytes) else {
                            continue;
                        };
                        if subnet.route_table_id == route_table_id {
                            return Ok(Out::HasSubnetAssociations);
                        }
                    }

                    let by_name_key =
                        keys::route_table_by_vpc_name_key(route_table.vpc_id, &route_table.name);
                    let in_vpc_key =
                        keys::route_table_in_vpc_key(route_table.vpc_id, route_table.id);
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_vpc_key);
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) => Ok(()),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::Main) => Err(StoreError::Conflict(format!(
                "route table {route_table_id} is a main route table; delete the vpc instead"
            ))),
            Ok(Out::HasRoutes) => Err(StoreError::Conflict(format!(
                "route table {route_table_id} still has routes"
            ))),
            Ok(Out::HasSubnetAssociations) => Err(StoreError::Conflict(format!(
                "route table {route_table_id} is still associated with subnets"
            ))),
            Ok(Out::Corrupt(e)) => {
                Err(StoreError::Backend(format!("deserialize route table: {e}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn create_route(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        route_table_id: Uuid,
        req: NewRoute,
    ) -> Result<Route, StoreError> {
        validate::name("route", &req.name)?;
        let destination = crate::types::canonical_ip_network(req.destination);
        let route_table_key = keys::route_table_by_id_key(route_table_id);
        let vpc_key = keys::vpc_by_id_key(vpc_id);
        let by_destination_key = keys::route_by_table_destination_key(route_table_id, destination);
        let route_id = Uuid::new_v4();
        let by_id_key = keys::route_by_id_key(route_id);
        let in_table_key = keys::route_in_table_key(route_table_id, route_id);
        let id_str = route_id.to_string();

        if let RouteTarget::NatGateway { nat_gateway_id } = &req.target {
            let nat = self.read_nat_gateway_record(*nat_gateway_id).await?;
            keys::validate_nat_gateway_route_target(&nat, tenant_id, project_id, vpc_id)?;
        }

        enum Outcome {
            Created(Route),
            RouteTableMissingOrWrongParent,
            DestinationFamilyMissing,
            DestinationTaken,
            SerializeFailed(String),
        }

        let req_for_txn = req.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let route_table_key = route_table_key.clone();
                let vpc_key = vpc_key.clone();
                let by_destination_key = by_destination_key.clone();
                let by_id_key = by_id_key.clone();
                let in_table_key = in_table_key.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                let req = req_for_txn.clone();
                async move {
                    let route_table_bytes = match tr.get(&route_table_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::RouteTableMissingOrWrongParent),
                    };
                    let route_table: RouteTable = match serde_json::from_slice(&route_table_bytes) {
                        Ok(rt) => rt,
                        Err(_) => return Ok(Outcome::RouteTableMissingOrWrongParent),
                    };
                    if route_table.tenant_id != tenant_id
                        || route_table.project_id != project_id
                        || route_table.vpc_id != vpc_id
                    {
                        return Ok(Outcome::RouteTableMissingOrWrongParent);
                    }

                    let vpc_bytes = match tr.get(&vpc_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::RouteTableMissingOrWrongParent),
                    };
                    let vpc: Vpc = match serde_json::from_slice(&vpc_bytes) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::RouteTableMissingOrWrongParent),
                    };
                    if !crate::types::route_destination_family_present(&vpc, destination) {
                        return Ok(Outcome::DestinationFamilyMissing);
                    }
                    if tr.get(&by_destination_key, false).await?.is_some() {
                        return Ok(Outcome::DestinationTaken);
                    }

                    let route = Route {
                        id: route_id,
                        tenant_id,
                        project_id,
                        vpc_id,
                        route_table_id,
                        name: req.name,
                        description: req.description.unwrap_or_default(),
                        destination,
                        target: req.target,
                        created_at: Utc::now(),
                    };
                    let value = match serde_json::to_vec(&route) {
                        Ok(v) => v,
                        Err(e) => return Ok(Outcome::SerializeFailed(e.to_string())),
                    };
                    tr.set(&by_id_key, &value);
                    tr.set(&by_destination_key, &id_bytes);
                    tr.set(&in_table_key, b"");
                    Ok(Outcome::Created(route))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(route)) => Ok(route),
            Ok(Outcome::RouteTableMissingOrWrongParent) => Err(StoreError::NotFound),
            Ok(Outcome::DestinationFamilyMissing) => Err(StoreError::Conflict(format!(
                "route destination {destination} uses an address family not present on vpc {vpc_id}"
            ))),
            Ok(Outcome::DestinationTaken) => Err(StoreError::Conflict(format!(
                "route destination {destination} already exists in route table {route_table_id}"
            ))),
            Ok(Outcome::SerializeFailed(e)) => {
                Err(StoreError::Backend(format!("serialize route: {e}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn get_route(&self, route_id: Uuid) -> Result<Route, StoreError> {
        let key = keys::route_by_id_key(route_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("route"))
    }

    async fn list_routes_in_table(&self, route_table_id: Uuid) -> Result<Vec<Route>, StoreError> {
        let prefix = keys::route_in_table_prefix(route_table_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("route index uuid: {e}")))?;
            match self.get_route(id).await {
                Ok(route) => out.push(route),
                Err(StoreError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn delete_route(&self, route_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::route_by_id_key(route_id);

        enum Out {
            Deleted,
            Vanished,
            Corrupt(String),
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
                    let route: Route = match serde_json::from_slice(&bytes) {
                        Ok(r) => r,
                        Err(e) => return Ok(Out::Corrupt(e.to_string())),
                    };
                    tr.clear(&by_id_key);
                    tr.clear(&keys::route_by_table_destination_key(
                        route.route_table_id,
                        route.destination,
                    ));
                    tr.clear(&keys::route_in_table_key(route.route_table_id, route.id));
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) => Ok(()),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::Corrupt(e)) => Err(StoreError::Backend(format!("deserialize route: {e}"))),
            Err(e) => Err(e.into()),
        }
    }

    async fn create_nat_gateway(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewNatGateway,
    ) -> Result<NatGateway, StoreError> {
        validate::name("nat_gateway", &req.name)?;
        let vpc_check_key = keys::vpc_by_id_key(vpc_id);
        let by_name_key = keys::nat_gateway_by_vpc_name_key(vpc_id, &req.name);
        let alloc_v4_prefix = keys::floating_ip_alloc_v4_prefix().to_vec();
        let alloc_v6_prefix = keys::floating_ip_alloc_v6_prefix().to_vec();
        let (v4_begin, v4_end) = prefix_range(&alloc_v4_prefix);
        let (v6_begin, v6_end) = prefix_range(&alloc_v6_prefix);
        let v4_prefix_len = alloc_v4_prefix.len();
        let v6_prefix_len = alloc_v6_prefix.len();

        let nat_gateway_id = Uuid::new_v4();
        let by_id_key = keys::nat_gateway_by_id_key(nat_gateway_id);
        let in_vpc_key = keys::nat_gateway_in_vpc_key(vpc_id, nat_gateway_id);
        let id_str = nat_gateway_id.to_string();

        enum Outcome {
            Created(Box<NatGatewayRecord>),
            VpcMissingOrWrongParent,
            NameTaken,
            PoolExhausted,
            SerializeFailed(String),
        }

        let req_for_txn = req.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let vpc_check_key = vpc_check_key.clone();
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_vpc_key = in_vpc_key.clone();
                let v4_begin = v4_begin.clone();
                let v4_end = v4_end.clone();
                let v6_begin = v6_begin.clone();
                let v6_end = v6_end.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                let req = req_for_txn.clone();
                async move {
                    let vpc_bytes = match tr.get(&vpc_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::VpcMissingOrWrongParent),
                    };
                    let vpc: Vpc = match serde_json::from_slice(&vpc_bytes) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::VpcMissingOrWrongParent),
                    };
                    if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
                        return Ok(Outcome::VpcMissingOrWrongParent);
                    }
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }

                    let public_address: std::net::IpAddr = match req.family {
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
                    let record = NatGatewayRecord {
                        id: nat_gateway_id,
                        tenant_id,
                        project_id,
                        vpc_id,
                        name: req.name.clone(),
                        description: req.description.unwrap_or_default(),
                        family: req.family,
                        public_address,
                        edge_cluster_id: None,
                        desired_generation: 1,
                        created_at: now,
                        updated_at: now,
                    };
                    let value = match serde_json::to_vec(&record) {
                        Ok(v) => v,
                        Err(e) => return Ok(Outcome::SerializeFailed(e.to_string())),
                    };
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&in_vpc_key, b"");
                    let holder = keys::public_ip_holder_value(NetworkResourceId::NatGateway {
                        id: nat_gateway_id,
                    });
                    match public_address {
                        std::net::IpAddr::V4(v4) => {
                            tr.set(&keys::floating_ip_alloc_v4_key(v4), &holder);
                        }
                        std::net::IpAddr::V6(v6) => {
                            tr.set(&keys::floating_ip_alloc_v6_key(v6), &holder);
                        }
                    }
                    Ok(Outcome::Created(Box::new(record)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(record)) => Ok((*record).into_view(Vec::new())),
            Ok(Outcome::VpcMissingOrWrongParent) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "nat gateway with name {:?} already exists in vpc {vpc_id}",
                req.name
            ))),
            Ok(Outcome::PoolExhausted) => {
                Err(StoreError::Backend("public ip pool exhausted".to_string()))
            }
            Ok(Outcome::SerializeFailed(e)) => {
                Err(StoreError::Backend(format!("serialize nat gateway: {e}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn get_nat_gateway(&self, nat_gateway_id: Uuid) -> Result<NatGateway, StoreError> {
        let record = self.read_nat_gateway_record(nat_gateway_id).await?;
        let mut rows = self.list_network_realizations(record.resource_id()).await?;
        if let Some(edge_cluster_id) = record.edge_cluster_id {
            rows.extend(
                self.list_network_realizations(NetworkResourceId::EdgeCluster {
                    id: edge_cluster_id,
                })
                .await?,
            );
        }
        Ok(record.into_view(rows))
    }

    async fn list_nat_gateways_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<NatGateway>, StoreError> {
        let prefix = keys::nat_gateway_in_vpc_prefix(vpc_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("nat gateway index uuid: {e}")))?;
            match self.get_nat_gateway(id).await {
                Ok(nat) => out.push(nat),
                Err(StoreError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn delete_nat_gateway(&self, nat_gateway_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::nat_gateway_by_id_key(nat_gateway_id);

        enum Out {
            Deleted,
            Vanished,
            HasRoutes,
            Corrupt(String),
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
                    let record: NatGatewayRecord = match serde_json::from_slice(&bytes) {
                        Ok(n) => n,
                        Err(e) => return Ok(Out::Corrupt(e.to_string())),
                    };
                    let route_prefix = keys::route_by_id_prefix();
                    let (route_begin, route_end) = prefix_range(&route_prefix);
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(route_begin),
                        end: KeySelector::first_greater_or_equal(route_end),
                        ..RangeOption::default()
                    };
                    let route_kvs = tr.get_range(&opt, 1, false).await?;
                    for kv in route_kvs.iter() {
                        let route: Route = match serde_json::from_slice(kv.value()) {
                            Ok(route) => route,
                            Err(e) => return Ok(Out::Corrupt(e.to_string())),
                        };
                        if matches!(
                            route.target,
                            RouteTarget::NatGateway {
                                nat_gateway_id: id
                            } if id == nat_gateway_id
                        ) {
                            return Ok(Out::HasRoutes);
                        }
                    }
                    let by_name_key =
                        keys::nat_gateway_by_vpc_name_key(record.vpc_id, &record.name);
                    let in_vpc_key = keys::nat_gateway_in_vpc_key(record.vpc_id, record.id);
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_vpc_key);
                    match record.public_address {
                        std::net::IpAddr::V4(v4) => {
                            tr.clear(&keys::floating_ip_alloc_v4_key(v4));
                        }
                        std::net::IpAddr::V6(v6) => {
                            tr.clear(&keys::floating_ip_alloc_v6_key(v6));
                        }
                    }
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) => Ok(()),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::HasRoutes) => Err(StoreError::Conflict(format!(
                "nat gateway {nat_gateway_id} is still referenced by routes"
            ))),
            Ok(Out::Corrupt(e)) => {
                Err(StoreError::Backend(format!("deserialize nat gateway: {e}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn create_edge_cluster(&self, req: NewEdgeCluster) -> Result<EdgeCluster, StoreError> {
        validate::name("edge_cluster", &req.name)?;
        validate_edge_cluster_bound_resource_shape(req.kind, &req.bound_resources)?;

        let edge_cluster_id = Uuid::new_v4();
        let by_id_key = keys::edge_cluster_by_id_key(edge_cluster_id);
        let by_name_key = keys::edge_cluster_by_name_key(&req.name);
        let all_key = keys::edge_cluster_all_key(edge_cluster_id);
        let id_str = edge_cluster_id.to_string();

        enum Outcome {
            Created(Box<EdgeClusterRecord>),
            NameTaken,
            ResourceMissing,
            ResourceCorrupt(String),
            SerializeFailed(String),
        }

        let req_for_txn = req.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let all_key = all_key.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                let req = req_for_txn.clone();
                async move {
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }

                    let now = Utc::now();
                    for resource in &req.bound_resources {
                        let resource_key = edge_cluster_resource_record_key(*resource);
                        let bytes = match tr.get(&resource_key, false).await? {
                            Some(bytes) => bytes,
                            None => return Ok(Outcome::ResourceMissing),
                        };
                        if let EdgeClusterResource::NatGateway { .. } = resource {
                            let mut nat: NatGatewayRecord = match serde_json::from_slice(&bytes) {
                                Ok(nat) => nat,
                                Err(e) => return Ok(Outcome::ResourceCorrupt(e.to_string())),
                            };
                            nat.edge_cluster_id = Some(edge_cluster_id);
                            nat.updated_at = now;
                            let value = match serde_json::to_vec(&nat) {
                                Ok(v) => v,
                                Err(e) => return Ok(Outcome::SerializeFailed(e.to_string())),
                            };
                            tr.set(&resource_key, &value);
                        }
                    }

                    let record = EdgeClusterRecord {
                        id: edge_cluster_id,
                        name: req.name.clone(),
                        kind: req.kind,
                        bound_resources: req.bound_resources.clone(),
                        instances: req.instances.clone(),
                        desired_generation: 1,
                        created_at: now,
                        updated_at: now,
                    };
                    let value = match serde_json::to_vec(&record) {
                        Ok(v) => v,
                        Err(e) => return Ok(Outcome::SerializeFailed(e.to_string())),
                    };
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&all_key, b"");
                    for resource in &record.bound_resources {
                        let key = keys::edge_cluster_by_resource_key(*resource, record.id);
                        tr.set(&key, b"");
                    }
                    Ok(Outcome::Created(Box::new(record)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(record)) => Ok((*record).into_view(Vec::new())),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "edge cluster with name {:?} already exists",
                req.name
            ))),
            Ok(Outcome::ResourceMissing) => Err(StoreError::NotFound),
            Ok(Outcome::ResourceCorrupt(e)) => Err(StoreError::Backend(format!(
                "deserialize edge cluster resource: {e}"
            ))),
            Ok(Outcome::SerializeFailed(e)) => {
                Err(StoreError::Backend(format!("serialize edge cluster: {e}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn get_edge_cluster(&self, edge_cluster_id: Uuid) -> Result<EdgeCluster, StoreError> {
        let record = self.read_edge_cluster_record(edge_cluster_id).await?;
        let rows = self.list_network_realizations(record.resource_id()).await?;
        Ok(record.into_view(rows))
    }

    async fn list_edge_clusters(&self) -> Result<Vec<EdgeCluster>, StoreError> {
        let prefix = keys::edge_cluster_all_prefix().to_vec();
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("edge cluster index uuid: {e}")))?;
            match self.get_edge_cluster(id).await {
                Ok(cluster) => out.push(cluster),
                Err(StoreError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn list_edge_clusters_for_resource(
        &self,
        resource: EdgeClusterResource,
    ) -> Result<Vec<EdgeCluster>, StoreError> {
        let prefix = keys::edge_cluster_by_resource_prefix(resource);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("edge cluster index uuid: {e}")))?;
            match self.get_edge_cluster(id).await {
                Ok(cluster) => out.push(cluster),
                Err(StoreError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn delete_edge_cluster(&self, edge_cluster_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::edge_cluster_by_id_key(edge_cluster_id);

        enum Out {
            Deleted,
            Vanished,
            Corrupt(String),
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
                    let record: EdgeClusterRecord = match serde_json::from_slice(&bytes) {
                        Ok(c) => c,
                        Err(e) => return Ok(Out::Corrupt(e.to_string())),
                    };
                    tr.clear(&by_id_key);
                    tr.clear(&keys::edge_cluster_by_name_key(&record.name));
                    tr.clear(&keys::edge_cluster_all_key(record.id));
                    let now = Utc::now();
                    for resource in &record.bound_resources {
                        tr.clear(&keys::edge_cluster_by_resource_key(*resource, record.id));
                        if let EdgeClusterResource::NatGateway { .. } = resource {
                            let resource_key = edge_cluster_resource_record_key(*resource);
                            if let Some(bytes) = tr.get(&resource_key, false).await? {
                                let mut nat: NatGatewayRecord = match serde_json::from_slice(&bytes)
                                {
                                    Ok(nat) => nat,
                                    Err(e) => return Ok(Out::Corrupt(e.to_string())),
                                };
                                if nat.edge_cluster_id == Some(record.id) {
                                    nat.edge_cluster_id = None;
                                    nat.updated_at = now;
                                    let value = match serde_json::to_vec(&nat) {
                                        Ok(v) => v,
                                        Err(e) => return Ok(Out::Corrupt(e.to_string())),
                                    };
                                    tr.set(&resource_key, &value);
                                }
                            }
                        }
                    }
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) => Ok(()),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::Corrupt(e)) => Err(StoreError::Backend(format!(
                "deserialize edge cluster: {e}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn create_ssh_key_public(
        &self,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        validate::name("ssh_key", &req.name)?;
        let scope = SshKeyScope::Public;
        let by_name_key = keys::ssh_key_by_public_name_key(&req.name);
        let by_fp_key = keys::ssh_key_by_public_fp_key(&fingerprint);
        let in_scope_key_for = |id: Uuid| keys::ssh_key_in_public_key(id);
        self.create_ssh_key_inner(
            scope,
            req,
            fingerprint,
            None,
            by_name_key,
            by_fp_key,
            in_scope_key_for,
            "public",
        )
        .await
    }

    async fn create_ssh_key_silo(
        &self,
        silo_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        validate::name("ssh_key", &req.name)?;
        let scope = SshKeyScope::Silo { silo_id };
        let by_name_key = keys::ssh_key_by_silo_name_key(silo_id, &req.name);
        let by_fp_key = keys::ssh_key_by_silo_fp_key(silo_id, &fingerprint);
        let parent_check_key = keys::silo_by_id_key(silo_id);
        let in_scope_key_for = move |id: Uuid| keys::ssh_key_in_silo_key(silo_id, id);
        self.create_ssh_key_inner(
            scope,
            req,
            fingerprint,
            Some(parent_check_key),
            by_name_key,
            by_fp_key,
            in_scope_key_for,
            "silo",
        )
        .await
    }

    async fn create_ssh_key_tenant(
        &self,
        tenant_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        validate::name("ssh_key", &req.name)?;
        let scope = SshKeyScope::Tenant { tenant_id };
        let by_name_key = keys::ssh_key_by_tenant_name_key(tenant_id, &req.name);
        let by_fp_key = keys::ssh_key_by_tenant_fp_key(tenant_id, &fingerprint);
        let parent_check_key = keys::tenant_by_id_key(tenant_id);
        let in_scope_key_for = move |id: Uuid| keys::ssh_key_in_tenant_key(tenant_id, id);
        self.create_ssh_key_inner(
            scope,
            req,
            fingerprint,
            Some(parent_check_key),
            by_name_key,
            by_fp_key,
            in_scope_key_for,
            "tenant",
        )
        .await
    }

    async fn create_ssh_key_project(
        &self,
        project_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        validate::name("ssh_key", &req.name)?;
        let scope = SshKeyScope::Project { project_id };
        let by_name_key = keys::ssh_key_by_project_name_key(project_id, &req.name);
        let by_fp_key = keys::ssh_key_by_project_fp_key(project_id, &fingerprint);
        let parent_check_key = keys::project_by_id_key(project_id);
        let in_scope_key_for = move |id: Uuid| keys::ssh_key_in_project_key(project_id, id);
        self.create_ssh_key_inner(
            scope,
            req,
            fingerprint,
            Some(parent_check_key),
            by_name_key,
            by_fp_key,
            in_scope_key_for,
            "project",
        )
        .await
    }

    async fn create_ssh_key_user(
        &self,
        user_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        validate::name("ssh_key", &req.name)?;
        let scope = SshKeyScope::User { user_id };
        let by_name_key = keys::ssh_key_by_user_name_key(user_id, &req.name);
        let by_fp_key = keys::ssh_key_by_user_fp_key(user_id, &fingerprint);
        let parent_check_key = keys::user_by_id_key(user_id);
        let in_scope_key_for = move |id: Uuid| keys::ssh_key_by_user_idx_key(user_id, id);
        self.create_ssh_key_inner(
            scope,
            req,
            fingerprint,
            Some(parent_check_key),
            by_name_key,
            by_fp_key,
            in_scope_key_for,
            "user",
        )
        .await
    }

    async fn get_ssh_key(&self, key_id: Uuid) -> Result<SshKey, StoreError> {
        let key = keys::ssh_key_by_id_key(key_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("ssh key"))
    }

    async fn list_ssh_keys_public(&self) -> Result<Vec<SshKey>, StoreError> {
        let prefix = keys::ssh_key_in_public_prefix();
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_ssh_keys_in_silo(&self, silo_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = keys::ssh_key_in_silo_prefix(silo_id);
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_ssh_keys_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = keys::ssh_key_in_tenant_prefix(tenant_id);
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_ssh_keys_in_project(&self, project_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = keys::ssh_key_in_project_prefix(project_id);
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_ssh_keys_for_user(&self, user_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = keys::ssh_key_by_user_idx_prefix(user_id);
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_visible_ssh_keys_in_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<SshKey>, StoreError> {
        let tenant_bytes = self
            .read_bytes(&keys::tenant_by_id_key(tenant_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let tenant: Tenant = serde_json::from_slice(&tenant_bytes)
            .map_err(de_err("tenant"))?;
        let mut out = self.list_ssh_keys_public().await?;
        out.extend(self.list_ssh_keys_in_silo(tenant.silo_id).await?);
        out.extend(self.list_ssh_keys_in_tenant(tenant_id).await?);
        Ok(out)
    }

    async fn list_visible_ssh_keys_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<SshKey>, StoreError> {
        let project_bytes = self
            .read_bytes(&keys::project_by_id_key(project_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let project: Project = serde_json::from_slice(&project_bytes)
            .map_err(de_err("project"))?;
        let tenant_bytes = self
            .read_bytes(&keys::tenant_by_id_key(project.tenant_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let tenant: Tenant = serde_json::from_slice(&tenant_bytes)
            .map_err(de_err("tenant"))?;
        let mut out = self.list_ssh_keys_public().await?;
        out.extend(self.list_ssh_keys_in_silo(tenant.silo_id).await?);
        out.extend(self.list_ssh_keys_in_tenant(project.tenant_id).await?);
        out.extend(self.list_ssh_keys_in_project(project_id).await?);
        Ok(out)
    }

    async fn delete_ssh_key(&self, key_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::ssh_key_by_id_key(key_id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(DelOut::Vanished),
                    };
                    let key: SshKey =
                        serde_json::from_slice(&bytes).map_err(txn_de_err("ssh key"))?;
                    let (by_name_key, by_fp_key, in_scope_key) = match &key.scope {
                        SshKeyScope::Public => (
                            keys::ssh_key_by_public_name_key(&key.name),
                            keys::ssh_key_by_public_fp_key(&key.fingerprint),
                            keys::ssh_key_in_public_key(key.id),
                        ),
                        SshKeyScope::Silo { silo_id } => (
                            keys::ssh_key_by_silo_name_key(*silo_id, &key.name),
                            keys::ssh_key_by_silo_fp_key(*silo_id, &key.fingerprint),
                            keys::ssh_key_in_silo_key(*silo_id, key.id),
                        ),
                        SshKeyScope::Tenant { tenant_id } => (
                            keys::ssh_key_by_tenant_name_key(*tenant_id, &key.name),
                            keys::ssh_key_by_tenant_fp_key(*tenant_id, &key.fingerprint),
                            keys::ssh_key_in_tenant_key(*tenant_id, key.id),
                        ),
                        SshKeyScope::Project { project_id } => (
                            keys::ssh_key_by_project_name_key(*project_id, &key.name),
                            keys::ssh_key_by_project_fp_key(*project_id, &key.fingerprint),
                            keys::ssh_key_in_project_key(*project_id, key.id),
                        ),
                        SshKeyScope::User { user_id } => (
                            keys::ssh_key_by_user_name_key(*user_id, &key.name),
                            keys::ssh_key_by_user_fp_key(*user_id, &key.fingerprint),
                            keys::ssh_key_by_user_idx_key(*user_id, key.id),
                        ),
                    };
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&by_fp_key);
                    tr.clear(&in_scope_key);
                    Ok(DelOut::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DelOut::Deleted) => Ok(()),
            Ok(DelOut::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn create_image_public(&self, req: NewImage) -> Result<Image, StoreError> {
        validate::name("image", &req.name)?;
        let scope = ImageScope::Public;
        let by_name_key = keys::image_by_public_name_key(&req.name);
        let in_scope_key_for = |id: Uuid| keys::image_in_public_key(id);
        self.create_image_inner(
            scope,
            req,
            None, // no parent existence check for Public.
            by_name_key,
            in_scope_key_for,
            "public",
        )
        .await
    }

    async fn create_image_silo(&self, silo_id: Uuid, req: NewImage) -> Result<Image, StoreError> {
        validate::name("image", &req.name)?;
        let scope = ImageScope::Silo { silo_id };
        let by_name_key = keys::image_by_silo_name_key(silo_id, &req.name);
        let parent_check_key = keys::silo_by_id_key(silo_id);
        let in_scope_key_for = move |id: Uuid| keys::image_in_silo_key(silo_id, id);
        self.create_image_inner(
            scope,
            req,
            Some(parent_check_key),
            by_name_key,
            in_scope_key_for,
            "silo",
        )
        .await
    }

    async fn create_image_tenant(
        &self,
        tenant_id: Uuid,
        req: NewImage,
    ) -> Result<Image, StoreError> {
        validate::name("image", &req.name)?;
        let scope = ImageScope::Tenant { tenant_id };
        let by_name_key = keys::image_by_tenant_name_key(tenant_id, &req.name);
        let parent_check_key = keys::tenant_by_id_key(tenant_id);
        let in_scope_key_for = move |id: Uuid| keys::image_in_tenant_key(tenant_id, id);
        self.create_image_inner(
            scope,
            req,
            Some(parent_check_key),
            by_name_key,
            in_scope_key_for,
            "tenant",
        )
        .await
    }

    async fn create_image_project(
        &self,
        project_id: Uuid,
        req: NewImage,
    ) -> Result<Image, StoreError> {
        validate::name("image", &req.name)?;
        let scope = ImageScope::Project { project_id };
        let by_name_key = keys::image_by_project_name_key(project_id, &req.name);
        let parent_check_key = keys::project_by_id_key(project_id);
        let in_scope_key_for = move |id: Uuid| keys::image_in_project_key(project_id, id);
        self.create_image_inner(
            scope,
            req,
            Some(parent_check_key),
            by_name_key,
            in_scope_key_for,
            "project",
        )
        .await
    }

    async fn create_image_user(&self, user_id: Uuid, req: NewImage) -> Result<Image, StoreError> {
        validate::name("image", &req.name)?;
        let scope = ImageScope::User { user_id };
        let by_name_key = keys::image_by_user_name_key(user_id, &req.name);
        let parent_check_key = keys::user_by_id_key(user_id);
        let in_scope_key_for = move |id: Uuid| keys::image_by_user_idx_key(user_id, id);
        self.create_image_inner(
            scope,
            req,
            Some(parent_check_key),
            by_name_key,
            in_scope_key_for,
            "user",
        )
        .await
    }

    async fn get_image(&self, image_id: Uuid) -> Result<Image, StoreError> {
        let key = keys::image_by_id_key(image_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("image"))
    }

    async fn list_images_public(&self) -> Result<Vec<Image>, StoreError> {
        let prefix = keys::image_in_public_prefix();
        self.list_images_via_index(prefix).await
    }

    async fn list_images_in_silo(&self, silo_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = keys::image_in_silo_prefix(silo_id);
        self.list_images_via_index(prefix).await
    }

    async fn list_images_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = keys::image_in_tenant_prefix(tenant_id);
        self.list_images_via_index(prefix).await
    }

    async fn list_images_in_project(&self, project_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = keys::image_in_project_prefix(project_id);
        self.list_images_via_index(prefix).await
    }

    async fn list_images_for_user(&self, user_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = keys::image_by_user_idx_prefix(user_id);
        self.list_images_via_index(prefix).await
    }

    async fn list_visible_images_in_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<Image>, StoreError> {
        // Tenant existence anchors the silo lookup; missing
        // tenant → NotFound (handler-side authorize_in_tenant
        // would already have 404'd a cross-tenant probe).
        let tenant_bytes = self
            .read_bytes(&keys::tenant_by_id_key(tenant_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let tenant: Tenant = serde_json::from_slice(&tenant_bytes)
            .map_err(de_err("tenant"))?;
        let mut out = self.list_images_public().await?;
        out.extend(self.list_images_in_silo(tenant.silo_id).await?);
        out.extend(self.list_images_in_tenant(tenant_id).await?);
        Ok(out)
    }

    async fn list_visible_images_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Image>, StoreError> {
        let project_bytes = self
            .read_bytes(&keys::project_by_id_key(project_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let project: Project = serde_json::from_slice(&project_bytes)
            .map_err(de_err("project"))?;
        let tenant_bytes = self
            .read_bytes(&keys::tenant_by_id_key(project.tenant_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let tenant: Tenant = serde_json::from_slice(&tenant_bytes)
            .map_err(de_err("tenant"))?;
        let mut out = self.list_images_public().await?;
        out.extend(self.list_images_in_silo(tenant.silo_id).await?);
        out.extend(self.list_images_in_tenant(project.tenant_id).await?);
        out.extend(self.list_images_in_project(project_id).await?);
        Ok(out)
    }

    async fn delete_image(&self, image_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::image_by_id_key(image_id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(DelOut::Vanished),
                    };
                    let image: Image =
                        serde_json::from_slice(&bytes).map_err(txn_de_err("image"))?;
                    let (by_name_key, in_scope_key) = match &image.scope {
                        ImageScope::Public => (
                            keys::image_by_public_name_key(&image.name),
                            keys::image_in_public_key(image.id),
                        ),
                        ImageScope::Silo { silo_id } => (
                            keys::image_by_silo_name_key(*silo_id, &image.name),
                            keys::image_in_silo_key(*silo_id, image.id),
                        ),
                        ImageScope::Tenant { tenant_id } => (
                            keys::image_by_tenant_name_key(*tenant_id, &image.name),
                            keys::image_in_tenant_key(*tenant_id, image.id),
                        ),
                        ImageScope::Project { project_id } => (
                            keys::image_by_project_name_key(*project_id, &image.name),
                            keys::image_in_project_key(*project_id, image.id),
                        ),
                        ImageScope::User { user_id } => (
                            keys::image_by_user_name_key(*user_id, &image.name),
                            keys::image_by_user_idx_key(*user_id, image.id),
                        ),
                    };
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_scope_key);
                    Ok(DelOut::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(DelOut::Deleted) => Ok(()),
            Ok(DelOut::Vanished) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn put_quota(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewQuota,
    ) -> Result<Quota, StoreError> {
        let project_check_key = keys::project_by_id_key(project_id);
        let quota_key = keys::quota_by_project_key(project_id);
        let quota = Quota {
            tenant_id,
            project_id,
            cpu_limit: req.cpu_limit,
            memory_bytes: req.memory_bytes,
            disk_bytes: req.disk_bytes,
            instance_limit: req.instance_limit,
            updated_at: Utc::now(),
        };
        let value = serde_json::to_vec(&quota)
            .map_err(ser_err("quota"))?;

        enum Outcome {
            Stored,
            ProjectMissingOrWrongTenant,
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
                        None => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    if project.tenant_id != tenant_id {
                        return Ok(Outcome::ProjectMissingOrWrongTenant);
                    }
                    tr.set(&quota_key, &value);
                    Ok(Outcome::Stored)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Stored) => Ok(quota),
            Ok(Outcome::ProjectMissingOrWrongTenant) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_quota(&self, tenant_id: Uuid, project_id: Uuid) -> Result<Quota, StoreError> {
        // Read project + quota inside a single transaction so the
        // tenant check is consistent with the read.
        let project_check_key = keys::project_by_id_key(project_id);
        let quota_key = keys::quota_by_project_key(project_id);

        enum Outcome {
            Found(Quota),
            ProjectMissingOrWrongTenant,
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
                        None => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    if project.tenant_id != tenant_id {
                        return Ok(Outcome::ProjectMissingOrWrongTenant);
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
            Ok(Outcome::ProjectMissingOrWrongTenant) | Ok(Outcome::QuotaUnset) => {
                Err(StoreError::NotFound)
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn delete_quota(&self, tenant_id: Uuid, project_id: Uuid) -> Result<(), StoreError> {
        let project_check_key = keys::project_by_id_key(project_id);
        let quota_key = keys::quota_by_project_key(project_id);

        enum Outcome {
            Deleted,
            ProjectMissingOrWrongTenant,
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
                        None => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    if project.tenant_id != tenant_id {
                        return Ok(Outcome::ProjectMissingOrWrongTenant);
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
            Ok(Outcome::ProjectMissingOrWrongTenant) | Ok(Outcome::QuotaUnset) => {
                Err(StoreError::NotFound)
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn create_instance(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewInstance,
    ) -> Result<InstanceCreateResult, StoreError> {
        validate::name("instance", &req.name)?;
        // All cross-resource reads + the IP allocation set scan +
        // the instance write + the NIC write + the IP-alloc index
        // writes happen in a single transaction. A concurrent
        // delete of any referenced resource aborts cleanly; a
        // concurrent NIC create that would race for the same IP
        // is serialized by FDB's optimistic concurrency.
        let tenant_check_key = keys::tenant_by_id_key(tenant_id);
        let project_check_key = keys::project_by_id_key(project_id);
        let image_check_key = keys::image_by_id_key(req.image_id);
        let subnet_check_key = keys::subnet_by_id_key(req.primary_subnet_id);
        let ssh_key_check_keys: Vec<(Uuid, Vec<u8>)> = req
            .ssh_key_ids
            .iter()
            .map(|id| (*id, keys::ssh_key_by_id_key(*id)))
            .collect();
        let by_name_key = keys::instance_by_project_name_key(project_id, &req.name);
        let alloc_v4_prefix = keys::nic_ip_alloc_v4_prefix(req.primary_subnet_id);
        let alloc_v6_prefix = keys::nic_ip_alloc_v6_prefix(req.primary_subnet_id);
        let (v4_begin, v4_end) = prefix_range(&alloc_v4_prefix);
        let (v6_begin, v6_end) = prefix_range(&alloc_v6_prefix);
        let v4_prefix_len = alloc_v4_prefix.len();
        let v6_prefix_len = alloc_v6_prefix.len();

        let instance_id = Uuid::new_v4();
        let nic_id = Uuid::new_v4();
        let disk_id = Uuid::new_v4();
        let by_id_key = keys::instance_by_id_key(instance_id);
        let in_project_key = keys::instance_in_project_key(project_id, instance_id);
        let nic_by_id_key = keys::nic_by_id_key(nic_id);
        let nic_in_instance_key = keys::nic_in_instance_key(instance_id, nic_id);
        let disk_by_id_key = keys::disk_by_id_key(disk_id);
        let disk_in_instance_key = keys::disk_in_instance_key(instance_id, disk_id);
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
                let v4_prefix = keys::nic_ip_alloc_v4_prefix(spec.subnet_id);
                let v6_prefix = keys::nic_ip_alloc_v6_prefix(spec.subnet_id);
                let (v4_begin, v4_end) = prefix_range(&v4_prefix);
                let (v6_begin, v6_end) = prefix_range(&v6_prefix);
                let nid = Uuid::new_v4();
                ExtraNicTxnPlan {
                    spec_subnet_id: spec.subnet_id,
                    name: spec.name.clone(),
                    nic_id: nid,
                    subnet_check_key: keys::subnet_by_id_key(spec.subnet_id),
                    v4_prefix_len: v4_prefix.len(),
                    v6_prefix_len: v6_prefix.len(),
                    v4_begin,
                    v4_end,
                    v6_begin,
                    v6_end,
                    nic_by_id_key: keys::nic_by_id_key(nid),
                    nic_in_instance_key: keys::nic_in_instance_key(instance_id, nid),
                }
            })
            .collect();

        enum Outcome {
            Created(Box<InstanceCreateResult>),
            TenantMissing,
            ProjectMissingOrWrongTenant,
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
                let tenant_check_key = tenant_check_key.clone();
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
                    // Tenant: needed for project ownership check.
                    // As of slice G both image and ssh-key are
                    // multi-scope; visibility is enforced by the
                    // API handler before invoking create_instance.
                    let tenant_bytes = match tr.get(&tenant_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::TenantMissing),
                    };
                    let _tenant: Tenant = match serde_json::from_slice(&tenant_bytes) {
                        Ok(t) => t,
                        Err(_) => return Ok(Outcome::TenantMissing),
                    };
                    // Project
                    let project_bytes = match tr.get(&project_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    if project.tenant_id != tenant_id {
                        return Ok(Outcome::ProjectMissingOrWrongTenant);
                    }
                    // Image (multi-scope as of slice F).
                    // Visibility is enforced by the API handler
                    // before invoking create_instance; the store
                    // only checks that the image record exists.
                    let image_bytes = match tr.get(&image_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::ImageMissingOrWrongSilo),
                    };
                    let image: Image = match serde_json::from_slice(&image_bytes) {
                        Ok(i) => i,
                        Err(_) => return Ok(Outcome::ImageMissingOrWrongSilo),
                    };
                    // Subnet
                    let subnet_bytes = match tr.get(&subnet_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::SubnetMissingOrWrongParent),
                    };
                    let subnet: Subnet = match serde_json::from_slice(&subnet_bytes) {
                        Ok(s) => s,
                        Err(_) => return Ok(Outcome::SubnetMissingOrWrongParent),
                    };
                    if subnet.tenant_id != tenant_id || subnet.project_id != project_id {
                        return Ok(Outcome::SubnetMissingOrWrongParent);
                    }
                    // SSH keys (multi-scope as of slice G).
                    // Visibility is enforced by the API handler
                    // before invoking create_instance; the store
                    // only checks that each key record exists.
                    for (_key_id, key_check_key) in &ssh_key_check_keys {
                        if tr.get(key_check_key, false).await?.is_none() {
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
                        tenant_id,
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
                        tenant_id,
                        project_id,
                        name: req.name.clone(),
                        description: req.description.unwrap_or_default(),
                        image_id: req.image_id,
                        brand: InstanceBrand::from_image(&image),
                        primary_subnet_id: req.primary_subnet_id,
                        ssh_key_ids: req.ssh_key_ids,
                        cpu: req.cpu,
                        memory_bytes: req.memory_bytes,
                        host_cn_uuid: None,
                        lifecycle: LifecycleState::Pending,
                        created_at: now,
                        updated_at: now,
                    };
                    let boot_disk = Disk {
                        id: disk_id,
                        tenant_id,
                        project_id,
                        instance_id,
                        name: "boot".to_string(),
                        description: format!("Boot disk for instance {}", instance.name),
                        kind: DiskKind::Boot,
                        size_bytes: default_boot_disk_size_bytes(&image),
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
                    // RFD 00007 AP-1c: image -> instance membership
                    // index. Image_id is fixed on the instance row;
                    // delete_instance clears the matching key.
                    tr.set(&keys::instance_in_image_key(req.image_id, instance_id), b"");
                    tr.set(&nic_by_id_key, &nic_value);
                    tr.set(&nic_in_instance_key, b"");
                    // RFD 00007 AP-1c: subnet -> nic membership and
                    // ip -> nic unique indexes for the primary NIC.
                    tr.set(&keys::nic_in_subnet_key(subnet.id, nic_id), b"");
                    tr.set(&disk_by_id_key, &disk_value);
                    tr.set(&disk_in_instance_key, b"");
                    if let Some(ip) = primary_ipv4 {
                        let alloc_key = keys::nic_ip_alloc_v4_key(subnet.id, ip);
                        tr.set(&alloc_key, b"");
                        tr.set(
                            &keys::nic_by_ip_key(std::net::IpAddr::V4(ip)),
                            nic_id.to_string().as_bytes(),
                        );
                    }
                    if let Some(ip) = primary_ipv6 {
                        let alloc_key = keys::nic_ip_alloc_v6_key(subnet.id, ip);
                        tr.set(&alloc_key, b"");
                        tr.set(
                            &keys::nic_by_ip_key(std::net::IpAddr::V6(ip)),
                            nic_id.to_string().as_bytes(),
                        );
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
                        if extra_subnet.tenant_id != tenant_id
                            || extra_subnet.project_id != project_id
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
                            tenant_id,
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
                        // RFD 00007 AP-1c: subnet/IP indexes for the
                        // extra NIC, same as the primary above.
                        tr.set(&keys::nic_in_subnet_key(extra_subnet.id, plan.nic_id), b"");
                        if let Some(ip) = extra_v4 {
                            let alloc_key = keys::nic_ip_alloc_v4_key(extra_subnet.id, ip);
                            tr.set(&alloc_key, b"");
                            tr.set(
                                &keys::nic_by_ip_key(std::net::IpAddr::V4(ip)),
                                plan.nic_id.to_string().as_bytes(),
                            );
                        }
                        if let Some(ip) = extra_v6 {
                            let alloc_key = keys::nic_ip_alloc_v6_key(extra_subnet.id, ip);
                            tr.set(&alloc_key, b"");
                            tr.set(
                                &keys::nic_by_ip_key(std::net::IpAddr::V6(ip)),
                                plan.nic_id.to_string().as_bytes(),
                            );
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
            Ok(Outcome::TenantMissing)
            | Ok(Outcome::ProjectMissingOrWrongTenant)
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
            Err(e) => Err(e.into()),
        }
    }

    async fn get_instance(&self, instance_id: Uuid) -> Result<Instance, StoreError> {
        let key = keys::instance_by_id_key(instance_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("instance"))
    }

    async fn list_instances_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Instance>, StoreError> {
        let prefix = keys::instance_in_project_prefix(project_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("instance index uuid: {e}")))?;
            let by_id_key = keys::instance_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let instance: Instance = serde_json::from_slice(&bytes)
                    .map_err(de_err("instance"))?;
                out.push(instance);
            }
        }
        Ok(out)
    }

    async fn set_instance_host_cn(
        &self,
        instance_id: Uuid,
        host_cn_uuid: Option<Uuid>,
    ) -> Result<Instance, StoreError> {
        let by_id_key = keys::instance_by_id_key(instance_id);

        enum Outcome {
            Updated(Box<Instance>),
            Vanished,
            Serialize,
        }
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::Vanished),
                    };
                    let mut instance: Instance = match serde_json::from_slice(&bytes) {
                        Ok(i) => i,
                        Err(_) => return Ok(Outcome::Vanished),
                    };
                    let previous = instance.host_cn_uuid;
                    instance.host_cn_uuid = host_cn_uuid;
                    instance.updated_at = Utc::now();
                    let value = match serde_json::to_vec(&instance) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::Serialize),
                    };
                    if let Some(old_host) = previous {
                        tr.clear(&keys::instance_in_host_cn_key(old_host, instance_id));
                    }
                    if let Some(new_host) = host_cn_uuid {
                        tr.set(&keys::instance_in_host_cn_key(new_host, instance_id), b"");
                    }
                    tr.set(&by_id_key, &value);
                    Ok(Outcome::Updated(Box::new(instance)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated(instance)) => Ok(*instance),
            Ok(Outcome::Vanished) => Err(StoreError::NotFound),
            Ok(Outcome::Serialize) => Err(StoreError::Backend(
                "serialize instance host placement".to_string(),
            )),
            Err(e) => Err(e.into()),
        }
    }

    async fn set_instance_brand(
        &self,
        instance_id: Uuid,
        brand: InstanceBrand,
    ) -> Result<(), StoreError> {
        let by_id_key = keys::instance_by_id_key(instance_id);

        enum Outcome {
            Updated,
            Vanished,
            Serialize,
        }
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::Vanished),
                    };
                    let mut instance: Instance = match serde_json::from_slice(&bytes) {
                        Ok(i) => i,
                        Err(_) => return Ok(Outcome::Vanished),
                    };
                    instance.brand = brand;
                    instance.updated_at = Utc::now();
                    let value = match serde_json::to_vec(&instance) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::Serialize),
                    };
                    tr.set(&by_id_key, &value);
                    Ok(Outcome::Updated)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated) => Ok(()),
            Ok(Outcome::Vanished) => Err(StoreError::NotFound),
            Ok(Outcome::Serialize) => Err(StoreError::Backend(
                "serialize instance brand backfill".to_string(),
            )),
            Err(e) => Err(e.into()),
        }
    }

    // RFD 00007 AP-1c: index-backed readers. Each method performs a
    // single FDB range read against the secondary index, parses the
    // uuid suffix(es), then point-reads the matching primary rows.
    async fn list_instances_by_image(&self, image_id: Uuid) -> Result<Vec<Instance>, StoreError> {
        let prefix = keys::instance_in_image_prefix(image_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("instance image index uuid: {e}")))?;
            let by_id_key = keys::instance_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let instance: Instance = serde_json::from_slice(&bytes)
                    .map_err(de_err("instance"))?;
                out.push(instance);
            }
        }
        Ok(out)
    }

    async fn list_instances_by_cn(&self, cn_uuid: Uuid) -> Result<Vec<Instance>, StoreError> {
        // Existing `instance/in_host_cn/<cn>/<inst>` index already
        // covers this; delegate.
        self.list_instances_for_cn(cn_uuid).await
    }

    async fn list_nics_by_subnet(&self, subnet_id: Uuid) -> Result<Vec<Nic>, StoreError> {
        let prefix = keys::nic_in_subnet_prefix(subnet_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("nic subnet index uuid: {e}")))?;
            let by_id_key = keys::nic_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let nic: Nic = serde_json::from_slice(&bytes)
                    .map_err(de_err("nic"))?;
                out.push(nic);
            }
        }
        Ok(out)
    }

    async fn find_nic_by_ip(&self, ip: std::net::IpAddr) -> Result<Nic, StoreError> {
        let key = keys::nic_by_ip_key(ip);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&bytes)
            .map_err(|e| StoreError::Backend(format!("nic by_ip index utf8: {e}")))?;
        let nic_id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("nic by_ip index uuid: {e}")))?;
        let nic_key = keys::nic_by_id_key(nic_id);
        let nic_bytes = self
            .read_bytes(&nic_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&nic_bytes)
            .map_err(de_err("nic"))
    }

    async fn find_dhcp_lease_by_mac(&self, mac: &str) -> Result<DhcpLease, StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let by_mac_key = keys::dhcp_lease_by_mac_key(&mac);
        let bytes = self
            .read_bytes(&by_mac_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        let vpc_str = std::str::from_utf8(&bytes)
            .map_err(|e| StoreError::Backend(format!("dhcp_lease by_mac utf8: {e}")))?;
        let vpc_id = Uuid::parse_str(vpc_str)
            .map_err(|e| StoreError::Backend(format!("dhcp_lease by_mac uuid: {e}")))?;
        let lease_key = keys::dhcp_lease_by_vpc_mac_key(vpc_id, &mac);
        let lease_bytes = self
            .read_bytes(&lease_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&lease_bytes)
            .map_err(de_err("dhcp lease"))
    }

    async fn list_instances_for_cn(&self, host_cn_uuid: Uuid) -> Result<Vec<Instance>, StoreError> {
        let prefix = keys::instance_in_host_cn_prefix(host_cn_uuid);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("instance host index uuid: {e}")))?;
            let by_id_key = keys::instance_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let instance: Instance = serde_json::from_slice(&bytes)
                    .map_err(de_err("instance"))?;
                out.push(instance);
            }
        }
        Ok(out)
    }

    async fn delete_instance(&self, instance_id: Uuid, force: bool) -> Result<(), StoreError> {
        let by_id_key = keys::instance_by_id_key(instance_id);
        let nic_prefix = keys::nic_in_instance_prefix(instance_id);
        let (nic_begin, nic_end) = prefix_range(&nic_prefix);
        let nic_prefix_len = nic_prefix.len();
        let disk_prefix = keys::disk_in_instance_prefix(instance_id);
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
                        let nic_key = keys::nic_by_id_key(nic_id);
                        let nic_in_instance_key =
                            keys::nic_in_instance_key(instance_id, nic_id);
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
                            tr.clear(&keys::nic_ip_alloc_v4_key(nic.subnet_id, ip));
                            // RFD 00007 AP-1c: drop the IP index entry.
                            tr.clear(&keys::nic_by_ip_key(std::net::IpAddr::V4(ip)));
                        }
                        if let Some(ip) = nic.primary_ipv6 {
                            tr.clear(&keys::nic_ip_alloc_v6_key(nic.subnet_id, ip));
                            tr.clear(&keys::nic_by_ip_key(std::net::IpAddr::V6(ip)));
                        }
                        // RFD 00007 AP-1c: drop the subnet membership index.
                        tr.clear(&keys::nic_in_subnet_key(nic.subnet_id, nic.id));
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
                        let dk = keys::disk_by_id_key(disk_id);
                        let dki = keys::disk_in_instance_key(instance_id, disk_id);
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
                        keys::floating_ip_in_project_prefix(instance.project_id);
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
                        let fk = keys::floating_ip_by_id_key(fip_id);
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
                    // RFD 00007 AP-1c: drop the image index entry.
                    tr.clear(&keys::instance_in_image_key(instance.image_id, instance.id));
                    if let Some(host_cn_uuid) = instance.host_cn_uuid {
                        tr.clear(&keys::instance_in_host_cn_key(host_cn_uuid, instance.id));
                    }
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
            Err(e) => Err(e.into()),
        }
    }

    async fn transition_instance_lifecycle(
        &self,
        instance_id: Uuid,
        expected_from: &[LifecycleStateKind],
        to: LifecycleState,
    ) -> Result<Instance, StoreError> {
        let by_id_key = keys::instance_by_id_key(instance_id);
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
            Err(e) => Err(e.into()),
        }
    }

    async fn get_nic(&self, nic_id: Uuid) -> Result<Nic, StoreError> {
        let key = keys::nic_by_id_key(nic_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("nic"))
    }

    async fn get_disk(&self, disk_id: Uuid) -> Result<Disk, StoreError> {
        let key = keys::disk_by_id_key(disk_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("disk"))
    }

    async fn list_disks_for_instance(&self, instance_id: Uuid) -> Result<Vec<Disk>, StoreError> {
        let prefix = keys::disk_in_instance_prefix(instance_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("disk index uuid: {e}")))?;
            let by_id_key = keys::disk_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let disk: Disk = serde_json::from_slice(&bytes)
                    .map_err(de_err("disk"))?;
                out.push(disk);
            }
        }
        Ok(out)
    }

    async fn list_nics_for_instance(&self, instance_id: Uuid) -> Result<Vec<Nic>, StoreError> {
        let prefix = keys::nic_in_instance_prefix(instance_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("nic index uuid: {e}")))?;
            let by_id_key = keys::nic_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let nic: Nic = serde_json::from_slice(&bytes)
                    .map_err(de_err("nic"))?;
                out.push(nic);
            }
        }
        Ok(out)
    }

    async fn create_floating_ip(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewFloatingIp,
    ) -> Result<FloatingIp, StoreError> {
        validate::name("floating_ip", &req.name)?;
        let project_check_key = keys::project_by_id_key(project_id);
        let by_name_key = keys::floating_ip_by_project_name_key(project_id, &req.name);
        let alloc_v4_prefix = keys::floating_ip_alloc_v4_prefix().to_vec();
        let alloc_v6_prefix = keys::floating_ip_alloc_v6_prefix().to_vec();
        let (v4_begin, v4_end) = prefix_range(&alloc_v4_prefix);
        let (v6_begin, v6_end) = prefix_range(&alloc_v6_prefix);
        let v4_prefix_len = alloc_v4_prefix.len();
        let v6_prefix_len = alloc_v6_prefix.len();

        let fip_id = Uuid::new_v4();
        let by_id_key = keys::floating_ip_by_id_key(fip_id);
        let in_project_key = keys::floating_ip_in_project_key(project_id, fip_id);
        let id_str = fip_id.to_string();

        enum Outcome {
            Created(Box<FloatingIp>),
            ProjectMissingOrWrongTenant,
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
                    // Project + same-tenant check.
                    let project_bytes = match tr.get(&project_check_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    let project: Project = match serde_json::from_slice(&project_bytes) {
                        Ok(p) => p,
                        Err(_) => return Ok(Outcome::ProjectMissingOrWrongTenant),
                    };
                    if project.tenant_id != tenant_id {
                        return Ok(Outcome::ProjectMissingOrWrongTenant);
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
                        tenant_id,
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
                    let holder =
                        keys::public_ip_holder_value(NetworkResourceId::FloatingIp { id: fip_id });
                    match address {
                        std::net::IpAddr::V4(v4) => {
                            tr.set(&keys::floating_ip_alloc_v4_key(v4), &holder);
                        }
                        std::net::IpAddr::V6(v6) => {
                            tr.set(&keys::floating_ip_alloc_v6_key(v6), &holder);
                        }
                    }
                    Ok(Outcome::Created(Box::new(fip)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(fip)) => Ok(*fip),
            Ok(Outcome::ProjectMissingOrWrongTenant) => Err(StoreError::NotFound),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "floating ip with name {:?} already exists in project {project_id}",
                req.name
            ))),
            Ok(Outcome::PoolExhausted) => Err(StoreError::Backend(
                "floating ip pool exhausted".to_string(),
            )),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_floating_ip(&self, fip_id: Uuid) -> Result<FloatingIp, StoreError> {
        let key = keys::floating_ip_by_id_key(fip_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("floating ip"))
    }

    async fn list_floating_ips_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<FloatingIp>, StoreError> {
        let prefix = keys::floating_ip_in_project_prefix(project_id);
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("floating ip index uuid: {e}")))?;
            let by_id_key = keys::floating_ip_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let fip: FloatingIp = serde_json::from_slice(&bytes)
                    .map_err(de_err("floating ip"))?;
                out.push(fip);
            }
        }
        Ok(out)
    }

    async fn delete_floating_ip(&self, fip_id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::floating_ip_by_id_key(fip_id);

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
                        keys::floating_ip_by_project_name_key(fip.project_id, &fip.name)
                            .into_bytes();
                    let in_project_key =
                        keys::floating_ip_in_project_key(fip.project_id, fip.id)
                            .into_bytes();
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_project_key);
                    match fip.address {
                        std::net::IpAddr::V4(v4) => {
                            tr.clear(&keys::floating_ip_alloc_v4_key(v4));
                        }
                        std::net::IpAddr::V6(v6) => {
                            tr.clear(&keys::floating_ip_alloc_v6_key(v6));
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
            Err(e) => Err(e.into()),
        }
    }

    async fn attach_floating_ip(
        &self,
        fip_id: Uuid,
        target_nic_id: Uuid,
    ) -> Result<FloatingIp, StoreError> {
        let by_id_key = keys::floating_ip_by_id_key(fip_id);
        let nic_check_key = keys::nic_by_id_key(target_nic_id);

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
                    if nic.tenant_id != fip.tenant_id || nic.project_id != fip.project_id {
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
            Err(e) => Err(e.into()),
        }
    }

    async fn detach_floating_ip(&self, fip_id: Uuid) -> Result<FloatingIp, StoreError> {
        let by_id_key = keys::floating_ip_by_id_key(fip_id);

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
            Err(e) => Err(e.into()),
        }
    }

    async fn enqueue_job(&self, req: NewJob) -> Result<ProvisioningJob, StoreError> {
        let counter_key = keys::job_seq_counter_key().to_vec();
        let job_id = Uuid::new_v4();
        let by_id_key = keys::job_by_id_key(job_id);
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
                    let pending_key = keys::job_pending_key(current_seq);
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
                    let value = serde_json::to_vec(&job).map_err(txn_ser_err("job"))?;
                    tr.set(&counter_key, &next_seq.to_be_bytes());
                    tr.set(&by_id_key, &value);
                    tr.set(&pending_key, &id_bytes);
                    Ok(job)
                }
            })
            .await;
        outcome.map_err(StoreError::from)
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
        let raws = raws.map_err(StoreError::from)?;

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
        let prefix = keys::job_pending_prefix().to_vec();
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
                    // Snapshot scan: the candidate range does not enter
                    // the read-conflict-range, so a concurrent claimer
                    // (or enqueuer) elsewhere in the pending prefix
                    // can't force this txn to retry. The follow-up
                    // `tr.get` and `tr.clear` on the chosen pending
                    // key still narrow the conflict window to that
                    // single job, so two claimers competing for the
                    // same job behave as before.
                    let kvs = tr.get_range(&opt, 1, true).await?;
                    let entries: Vec<(Vec<u8>, Vec<u8>)> = kvs
                        .iter()
                        .map(|kv| (kv.key().to_vec(), kv.value().to_vec()))
                        .collect();
                    drop(kvs);

                    for (pending_key, job_id_bytes) in entries {
                        let id_str = std::str::from_utf8(&job_id_bytes).map_err(txn_err("pending index value not utf8"))?;
                        let job_id = Uuid::parse_str(id_str).map_err(txn_err("pending index value not uuid"))?;
                        let by_id_key = keys::job_by_id_key(job_id);
                        // Serializable read on the chosen pending
                        // entry: brings the pending_key into the
                        // read-conflict-range so a concurrent claimer
                        // who cleared it from under us forces a retry.
                        if tr.get(&pending_key, false).await?.is_none() {
                            continue;
                        }
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
                            serde_json::from_slice(&bytes).map_err(txn_de_err("job"))?;
                        // Targeting check: skip mis-routed jobs
                        // without clearing their pending index — a
                        // different agent will pick them up.
                        if !targeting_matches(job.target_cn_uuid, claimer_cn) {
                            continue;
                        }
                        job.status = JobStatus::InProgress;
                        job.claimed_at = Some(Utc::now());
                        job.claimed_by = Some(claimed_by);
                        let value = serde_json::to_vec(&job).map_err(txn_ser_err("job"))?;
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
            Err(e) => Err(e.into()),
        }
    }

    async fn complete_job(
        &self,
        job_id: Uuid,
        outcome: JobOutcome,
    ) -> Result<ProvisioningJob, StoreError> {
        let by_id_key = keys::job_by_id_key(job_id);

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
            Err(e) => Err(e.into()),
        }
    }

    async fn get_job(&self, job_id: Uuid) -> Result<ProvisioningJob, StoreError> {
        let key = keys::job_by_id_key(job_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("job"))
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
        let raws = raws.map_err(StoreError::from)?;

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

    // ----- LM-1 live migrations -----
    //
    // FDB schema:
    //
    //   migration/by_id/<uuid>                       JSON MigrationRecord
    //   migration/by_instance/<inst>/<inv_ts>/<id>   id bytes (history)
    //   migration/active/<inst>                      id bytes (cross-handler guard)
    //   migration/progress/<id>/<seq:016x>           JSON MigrationProgressEvent
    //
    // `<inv_ts>` is `u64::MAX - created_at.timestamp_micros()` so a
    // forward range scan returns newest-first, matching the
    // `instance/by_id` history pattern elsewhere in this file.

    async fn create_migration(&self, req: NewMigration) -> Result<MigrationRecord, StoreError> {
        let now = chrono::Utc::now();
        let new_id = Uuid::new_v4();
        let record = MigrationRecord {
            id: new_id,
            instance_id: req.instance_id,
            tenant_id: req.tenant_id,
            project_id: req.project_id,
            source_cn: req.source_cn,
            target_cn: None,
            saga_id: None,
            phase: MigrationPhase::Begin,
            state: MigrationState::Begin,
            action_requested: req.action_requested,
            created_at: now,
            started_at: None,
            finished_at: None,
            error: None,
            reserved_nics: Vec::new(),
            source_filesystem_details: None,
            last_progress_seq: 0,
            disallow_retry: false,
            automatic: req.automatic,
        };
        let record_bytes = serde_json::to_vec(&record)
            .map_err(|e| StoreError::Backend(format!("encode MigrationRecord: {e}")))?;

        let by_id_key = keys::migration_by_id_key(record.id);
        let active_key = keys::migration_active_key(record.instance_id);
        let inv_ts = u64::MAX - (now.timestamp_micros() as u64);
        let by_instance_key = format!(
            "migration/by_instance/{}/{:016x}/{}",
            record.instance_id, inv_ts, record.id,
        )
        .into_bytes();
        let id_bytes = record.id.to_string().into_bytes();

        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let active_key = active_key.clone();
                let by_id_key = by_id_key.clone();
                let by_instance_key = by_instance_key.clone();
                let record_bytes = record_bytes.clone();
                let id_bytes = id_bytes.clone();
                async move {
                    if tr.get(&active_key, false).await?.is_some() {
                        return Err(FdbBindingError::CustomError(
                            "active-migration-conflict".to_string().into(),
                        ));
                    }
                    tr.set(&by_id_key, &record_bytes);
                    tr.set(&by_instance_key, &id_bytes);
                    tr.set(&active_key, &id_bytes);
                    Ok(())
                }
            })
            .await;
        match result {
            Ok(()) => Ok(record),
            Err(FdbBindingError::CustomError(e))
                if e.to_string().contains("active-migration-conflict") =>
            {
                Err(StoreError::Conflict(format!(
                    "instance {} already has an active migration",
                    req.instance_id,
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn get_migration(&self, migration_id: Uuid) -> Result<MigrationRecord, StoreError> {
        let key = keys::migration_by_id_key(migration_id);
        let raw: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|v| v.to_vec())) }
            })
            .await;
        let raw = raw.map_err(StoreError::from)?;
        let bytes = raw.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("decode MigrationRecord: {e}")))
    }

    async fn put_migration(&self, record: MigrationRecord) -> Result<MigrationRecord, StoreError> {
        let by_id_key = keys::migration_by_id_key(record.id);
        let bytes = serde_json::to_vec(&record)
            .map_err(|e| StoreError::Backend(format!("encode MigrationRecord: {e}")))?;
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let bytes = bytes.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Err(FdbBindingError::CustomError(
                            "migration-not-found".to_string().into(),
                        ));
                    }
                    tr.set(&by_id_key, &bytes);
                    Ok(())
                }
            })
            .await;
        match result {
            Ok(()) => Ok(record),
            Err(FdbBindingError::CustomError(e))
                if e.to_string().contains("migration-not-found") =>
            {
                Err(StoreError::NotFound)
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn list_migrations(
        &self,
        after_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<MigrationRecord>, StoreError> {
        // Phase 0: scan and sort. The hot path for migration
        // observation is per-instance history (a separate range);
        // fleet-wide list is operator-only.
        let prefix = b"migration/by_id/".to_vec();
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
        let raws = raws.map_err(StoreError::from)?;
        let mut rows: Vec<MigrationRecord> = raws
            .into_iter()
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect();
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        if let Some(cursor) = after_id
            && let Some(idx) = rows.iter().position(|r| r.id == cursor)
        {
            rows = rows.split_off(idx + 1);
        }
        rows.truncate(limit);
        Ok(rows)
    }

    async fn list_migrations_for_instance(
        &self,
        instance_id: Uuid,
    ) -> Result<Vec<MigrationRecord>, StoreError> {
        // Range scan of `migration/by_instance/<inst>/` returns ids
        // in newest-first order (inv_ts encoding); for each id fetch
        // the canonical record.
        let prefix = keys::migration_by_instance_prefix(instance_id);
        let (begin, end) = prefix_range(&prefix);
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
                    Ok(kvs
                        .iter()
                        .filter_map(|kv| std::str::from_utf8(kv.value()).ok().map(String::from))
                        .collect())
                }
            })
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;
        let mut rows = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            if let Ok(id) = Uuid::parse_str(&s)
                && let Ok(record) = self.get_migration(id).await
            {
                rows.push(record);
            }
        }
        Ok(rows)
    }

    async fn append_migration_progress(
        &self,
        migration_id: Uuid,
        mut event: MigrationProgressEvent,
    ) -> Result<MigrationProgressEvent, StoreError> {
        let record_key = keys::migration_by_id_key(migration_id);
        let event_clone = event.clone();
        let result: Result<MigrationProgressEvent, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let record_key = record_key.clone();
                let event_clone = event_clone.clone();
                async move {
                    let Some(bytes) = tr.get(&record_key, false).await? else {
                        return Err(FdbBindingError::CustomError(
                            "migration-not-found".to_string().into(),
                        ));
                    };
                    let mut record: MigrationRecord =
                        serde_json::from_slice(&bytes).map_err(txn_err("decode MigrationRecord"))?;
                    let next_seq = record.last_progress_seq.saturating_add(1);
                    record.last_progress_seq = next_seq;
                    let mut event_out = event_clone.clone();
                    event_out.seq = next_seq;
                    let record_bytes = serde_json::to_vec(&record).map_err(txn_err("encode MigrationRecord"))?;
                    let event_bytes = serde_json::to_vec(&event_out).map_err(txn_err("encode MigrationProgressEvent"))?;
                    let event_key = keys::migration_progress_key(migration_id, next_seq);
                    tr.set(&record_key, &record_bytes);
                    tr.set(&event_key, &event_bytes);
                    Ok(event_out)
                }
            })
            .await;
        match result {
            Ok(e) => {
                event = e;
                Ok(event)
            }
            Err(FdbBindingError::CustomError(s))
                if s.to_string().contains("migration-not-found") =>
            {
                Err(StoreError::NotFound)
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn list_migration_progress(
        &self,
        migration_id: Uuid,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<MigrationProgressEvent>, StoreError> {
        // Confirm the record exists so we can distinguish "no
        // progress yet" (empty Vec) from "no such migration".
        self.get_migration(migration_id).await?;
        let prefix = keys::migration_progress_prefix(migration_id);
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
        let raws = raws.map_err(StoreError::from)?;
        let rows: Vec<MigrationProgressEvent> = raws
            .into_iter()
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .filter(|e: &MigrationProgressEvent| e.seq > after_seq)
            .take(limit)
            .collect();
        Ok(rows)
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
            ClaimCodeExhausted,
        }

        let by_uuid_key = keys::cn_by_uuid_key(server_uuid);
        let window_key = keys::auto_approve_window_key().to_vec();
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
                        let existing: Cn = serde_json::from_slice(&bytes).map_err(txn_de_err("cn"))?;
                        let prev_state = existing.state;
                        match prev_state {
                            CnState::Approved => {
                                // Idempotent refresh: keep credentials,
                                // refresh sysinfo + hostname + last_seen.
                                let mut updated = existing;
                                updated.hostname = hostname;
                                updated.admin_ip = admin_ip;
                                updated.sysinfo = sysinfo;
                                updated.last_seen = Some(now);
                                let value = serde_json::to_vec(&updated).map_err(txn_ser_err("cn"))?;
                                tr.set(&by_uuid_key, &value);
                                return Ok(Outcome::Created(Box::new(updated)));
                            }
                            // Pending or Disabled: re-arm registration.
                            // Re-registering a Disabled CN is the
                            // supported "re-enable with fresh
                            // credentials" path -- the disable event
                            // stays in the audit chain. Drop the old
                            // by_claim / by_poll index entries; mint
                            // fresh ones (with collision check).
                            CnState::Pending | CnState::Disabled => {
                                if let Some(old_code) = &existing.claim_code {
                                    tr.clear(&keys::cn_by_claim_key(old_code));
                                }
                                tr.clear(&keys::cn_by_poll_key(&existing.poll_token));

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
                                    role: existing.role,
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
                                    // Re-registration drops to Pending;
                                    // console key regenerated on next
                                    // approval, port/cert re-reported by
                                    // the agent on this very register
                                    // call (the service layer threads
                                    // them through).
                                    console_listen_port: None,
                                    console_tls_spki_sha256: None,
                                    console_ticket_key: None,
                                    imds_token_key: None,
                                    migrate_ticket_key: None,
                                };
                                let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                                tr.set(&by_uuid_key, &value);
                                tr.set(&keys::cn_by_claim_key(&claim_code), &server_uuid_bytes);
                                tr.set(&keys::cn_by_poll_key(&poll_token), &server_uuid_bytes);
                                // Move the by_state membership to
                                // Pending (a no-op clear+set when the
                                // record was already Pending).
                                tr.clear(&keys::cn_by_state_key(prev_state, server_uuid));
                                tr.set(&keys::cn_by_state_key(CnState::Pending, server_uuid), b"");
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
                        role: CnRole::default(),
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
                        // Populated by the service layer's register
                        // handler from the agent's register payload /
                        // at approval.
                        console_listen_port: None,
                        console_tls_spki_sha256: None,
                        console_ticket_key: None,
                        imds_token_key: None,
                        migrate_ticket_key: None,
                    };
                    let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                    tr.set(&by_uuid_key, &value);
                    if let Some(code) = &claim_code {
                        tr.set(&keys::cn_by_claim_key(code), &server_uuid_bytes);
                    }
                    tr.set(&keys::cn_by_poll_key(&poll_token), &server_uuid_bytes);
                    tr.set(&keys::cn_by_state_key(state, server_uuid), b"");
                    Ok(Outcome::Created(Box::new(cn)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(cn)) => Ok(*cn),
            Ok(Outcome::ClaimCodeExhausted) => {
                Err(StoreError::Backend("claim code exhausted".to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn get_cn(&self, server_uuid: Uuid) -> Result<Cn, StoreError> {
        let key = keys::cn_by_uuid_key(server_uuid);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("cn"))
    }

    async fn get_cn_by_poll_token(&self, poll_token: &str) -> Result<Cn, StoreError> {
        let key = keys::cn_by_poll_key(poll_token);
        let id_bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("cn poll index not utf8: {e}")))?;
        let id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("cn poll index not uuid: {e}")))?;
        self.get_cn(id).await
    }

    async fn get_cn_by_claim_code(&self, code: &str) -> Result<Cn, StoreError> {
        let key = keys::cn_by_claim_key(code);
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
            let prefix = keys::cn_by_state_prefix(state);
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
                id_strs.map_err(StoreError::from)?;

            for s in id_strs {
                let id = Uuid::parse_str(&s)
                    .map_err(|e| StoreError::Backend(format!("cn state index uuid: {e}")))?;
                if !seen.insert(id) {
                    continue;
                }
                let by_id_key = keys::cn_by_uuid_key(id);
                if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                    let cn: Cn = serde_json::from_slice(&bytes)
                        .map_err(de_err("cn"))?;
                    out.push(cn);
                }
            }
        }
        Ok(out)
    }

    async fn set_cn_role(&self, server_uuid: Uuid, role: CnRole) -> Result<Cn, StoreError> {
        enum Outcome {
            Updated(Box<Cn>),
            NotFound,
        }

        let by_uuid_key = keys::cn_by_uuid_key(server_uuid);
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                async move {
                    let bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(txn_de_err("cn"))?;
                    cn.role = role;
                    let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                    tr.set(&by_uuid_key, &value);
                    Ok(Outcome::Updated(Box::new(cn)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated(cn)) => Ok(*cn),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn set_cn_console_endpoint(
        &self,
        server_uuid: Uuid,
        console_listen_port: Option<u16>,
        console_tls_spki_sha256: Option<[u8; 32]>,
    ) -> Result<(), StoreError> {
        enum Outcome {
            Updated,
            NotFound,
        }

        let by_uuid_key = keys::cn_by_uuid_key(server_uuid);
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                async move {
                    let bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(txn_de_err("cn"))?;
                    cn.console_listen_port = console_listen_port;
                    cn.console_tls_spki_sha256 = console_tls_spki_sha256;
                    let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                    tr.set(&by_uuid_key, &value);
                    Ok(Outcome::Updated)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated) => Ok(()),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn approve_cn(
        &self,
        server_uuid: Uuid,
        bound_api_key_id: Uuid,
        pending_credential: String,
        console_ticket_key: [u8; 32],
        imds_token_key: [u8; 32],
        migrate_ticket_key: [u8; 32],
        approved_at: chrono::DateTime<Utc>,
    ) -> Result<Cn, StoreError> {
        enum Outcome {
            Approved(Box<Cn>),
            NotFound,
            AlreadyBound,
        }

        let by_uuid_key = keys::cn_by_uuid_key(server_uuid);
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
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(txn_de_err("cn"))?;
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
                        tr.clear(&keys::cn_by_claim_key(old_code));
                    }
                    // If the record was Pending, drop the by_state/pending
                    // membership before adding the new approved one.
                    // (Auto-approve case: register_cn already wrote
                    // by_state/approved, so this is a no-op clear.)
                    tr.clear(&keys::cn_by_state_key(prev_state, server_uuid));

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
                    cn.console_ticket_key = Some(console_ticket_key);
                    cn.imds_token_key = Some(imds_token_key);
                    cn.migrate_ticket_key = Some(migrate_ticket_key);

                    let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                    tr.set(&by_uuid_key, &value);
                    tr.set(&keys::cn_by_state_key(CnState::Approved, server_uuid), b"");
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
            Err(e) => Err(e.into()),
        }
    }

    async fn consume_cn_pending_credential(
        &self,
        poll_token: &str,
    ) -> Result<Option<String>, StoreError> {
        let by_poll_key = keys::cn_by_poll_key(poll_token);

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
                    let id_str = std::str::from_utf8(&id_bytes).map_err(txn_err("cn poll index not utf8"))?;
                    let id = Uuid::parse_str(id_str).map_err(txn_err("cn poll index not uuid"))?;
                    let by_uuid_key = keys::cn_by_uuid_key(id);
                    let cn_bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&cn_bytes).map_err(txn_de_err("cn"))?;
                    let taken = cn.pending_credential.take();
                    if taken.is_some() {
                        let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                        tr.set(&by_uuid_key, &value);
                    }
                    Ok(Outcome::Consumed(taken))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Consumed(opt)) => Ok(opt),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn disable_cn(&self, server_uuid: Uuid) -> Result<Cn, StoreError> {
        enum Outcome {
            Disabled(Box<Cn>),
            NotFound,
        }

        let by_uuid_key = keys::cn_by_uuid_key(server_uuid);
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                async move {
                    let bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(txn_de_err("cn"))?;
                    if let Some(old_code) = &cn.claim_code {
                        tr.clear(&keys::cn_by_claim_key(old_code));
                    }
                    let old_state = cn.state;
                    tr.clear(&keys::cn_by_state_key(old_state, server_uuid));

                    cn.state = CnState::Disabled;
                    cn.claim_code = None;
                    cn.claim_code_expires_at = None;
                    cn.pending_credential = None;

                    let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                    tr.set(&by_uuid_key, &value);
                    tr.set(&keys::cn_by_state_key(CnState::Disabled, server_uuid), b"");
                    Ok(Outcome::Disabled(Box::new(cn)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Disabled(cn)) => Ok(*cn),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
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

        let by_uuid_key = keys::cn_by_uuid_key(server_uuid);
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_uuid_key = by_uuid_key.clone();
                async move {
                    let bytes = match tr.get(&by_uuid_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::NotFound),
                    };
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(txn_de_err("cn"))?;
                    cn.last_seen = Some(at);
                    let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                    tr.set(&by_uuid_key, &value);
                    Ok(Outcome::Updated)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated) => Ok(()),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
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

        let by_uuid_key = keys::cn_by_uuid_key(server_uuid);
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
                    let mut cn: Cn = serde_json::from_slice(&bytes).map_err(txn_de_err("cn"))?;
                    cn.last_status = Some(payload);
                    cn.last_seen = Some(at);
                    let value = serde_json::to_vec(&cn).map_err(txn_ser_err("cn"))?;
                    tr.set(&by_uuid_key, &value);
                    Ok(Outcome::Updated)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated) => Ok(()),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    // ------------------------------------------------------------------
    // Placement keyspaces (RFD 00005 PL-2)
    // ------------------------------------------------------------------

    async fn put_cn_capacity(&self, row: CnCapacity) -> Result<(), StoreError> {
        let key = keys::cn_capacity_key(row.server_uuid);
        let value = serde_json::to_vec(&row)
            .map_err(ser_err("cn_capacity"))?;
        self.db
            .run(|tr, _| {
                let key = key.clone();
                let value = value.clone();
                async move {
                    tr.set(&key, &value);
                    Ok(())
                }
            })
            .await
            .map_err(StoreError::from)
    }

    async fn get_cn_capacity(&self, server_uuid: Uuid) -> Result<CnCapacity, StoreError> {
        let key = keys::cn_capacity_key(server_uuid);
        let bytes: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;
        let bytes = bytes
            .map_err(StoreError::from)?
            .ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("cn_capacity"))
    }

    async fn list_cn_capacities(&self) -> Result<Vec<CnCapacity>, StoreError> {
        let prefix = keys::cn_capacity_prefix().to_vec();
        let (begin, end) = prefix_range(&prefix);
        let bytes_list: Result<Vec<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        mode: StreamingMode::WantAll,
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok(kvs.iter().map(|kv| kv.value().to_vec()).collect())
                }
            })
            .await;
        let bytes_list =
            bytes_list.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(bytes_list.len());
        for bytes in bytes_list {
            let v: CnCapacity = serde_json::from_slice(&bytes)
                .map_err(de_err("cn_capacity"))?;
            out.push(v);
        }
        out.sort_by_key(|c| c.server_uuid);
        Ok(out)
    }

    async fn put_cn_placement(&self, row: CnPlacement) -> Result<(), StoreError> {
        // D-Pl-5 pin invariant: validate inside the same FDB
        // transaction as the write so a concurrent edit can't sneak
        // past. Look up the tenant's silo row by id; reject if it
        // disagrees with the pinned silo.
        let placement_key = keys::cn_placement_key(row.server_uuid);
        let payload = serde_json::to_vec(&row)
            .map_err(ser_err("cn_placement"))?;

        enum Outcome {
            Wrote,
            Conflict(String),
        }

        let pin = match (row.pinned_tenant_uuid, row.pinned_silo_uuid) {
            (Some(t), Some(s)) => Some((t, s)),
            _ => None,
        };

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let placement_key = placement_key.clone();
                let payload = payload.clone();
                let pin = pin;
                async move {
                    if let Some((tenant_uuid, pinned_silo)) = pin {
                        let tenant_key = keys::tenant_by_id_key(tenant_uuid);
                        let tenant_bytes = match tr.get(&tenant_key, false).await? {
                            Some(b) => b,
                            None => {
                                return Ok(Outcome::Conflict(format!(
                                    "pinned tenant {tenant_uuid} not found"
                                )));
                            }
                        };
                        let tenant: Tenant =
                            serde_json::from_slice(&tenant_bytes).map_err(txn_de_err("tenant"))?;
                        if tenant.silo_id != pinned_silo {
                            return Ok(Outcome::Conflict(format!(
                                "pinned tenant {tenant_uuid} lives in silo {} but pinned silo is {pinned_silo}",
                                tenant.silo_id
                            )));
                        }
                    }
                    tr.set(&placement_key, &payload);
                    Ok(Outcome::Wrote)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Wrote) => Ok(()),
            Ok(Outcome::Conflict(reason)) => Err(StoreError::PinConflict { reason }),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_cn_placement(&self, server_uuid: Uuid) -> Result<CnPlacement, StoreError> {
        let key = keys::cn_placement_key(server_uuid);
        let bytes: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;
        match bytes.map_err(StoreError::from)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(de_err("cn_placement")),
            None => Ok(CnPlacement::fresh(server_uuid, Utc::now())),
        }
    }

    async fn list_cn_placements(&self) -> Result<Vec<CnPlacement>, StoreError> {
        let prefix = keys::cn_placement_prefix().to_vec();
        let (begin, end) = prefix_range(&prefix);
        let bytes_list: Result<Vec<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        mode: StreamingMode::WantAll,
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok(kvs.iter().map(|kv| kv.value().to_vec()).collect())
                }
            })
            .await;
        let bytes_list =
            bytes_list.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(bytes_list.len());
        for bytes in bytes_list {
            let v: CnPlacement = serde_json::from_slice(&bytes)
                .map_err(de_err("cn_placement"))?;
            out.push(v);
        }
        out.sort_by_key(|p| p.server_uuid);
        Ok(out)
    }

    async fn reserve_cn_capacity(&self, row: CnReservation) -> Result<(), StoreError> {
        let key = keys::cn_reservation_key(row.server_uuid, row.saga_id);
        let value = serde_json::to_vec(&row)
            .map_err(ser_err("cn_reservation"))?;
        let server_uuid = row.server_uuid;
        let saga_id = row.saga_id;

        enum Outcome {
            Wrote,
            AlreadyExists,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let value = value.clone();
                async move {
                    if tr.get(&key, false).await?.is_some() {
                        return Ok(Outcome::AlreadyExists);
                    }
                    tr.set(&key, &value);
                    Ok(Outcome::Wrote)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Wrote) => Ok(()),
            Ok(Outcome::AlreadyExists) => Err(StoreError::AlreadyExists(format!(
                "cn-reservation/{server_uuid}/{saga_id} already exists"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn release_cn_reservation(
        &self,
        server_uuid: Uuid,
        saga_id: Uuid,
    ) -> Result<(), StoreError> {
        let key = keys::cn_reservation_key(server_uuid, saga_id);

        enum Outcome {
            Deleted,
            NotFound,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move {
                    if tr.get(&key, false).await?.is_none() {
                        return Ok(Outcome::NotFound);
                    }
                    tr.clear(&key);
                    Ok(Outcome::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Deleted) => Ok(()),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_cn_reservations(
        &self,
        server_uuid: Option<Uuid>,
    ) -> Result<Vec<CnReservation>, StoreError> {
        let prefix = match server_uuid {
            Some(cn) => keys::cn_reservation_per_cn_prefix(cn),
            None => keys::cn_reservation_prefix().to_vec(),
        };
        let (begin, end) = prefix_range(&prefix);
        let bytes_list: Result<Vec<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        mode: StreamingMode::WantAll,
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok(kvs.iter().map(|kv| kv.value().to_vec()).collect())
                }
            })
            .await;
        let bytes_list =
            bytes_list.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(bytes_list.len());
        for bytes in bytes_list {
            let v: CnReservation = serde_json::from_slice(&bytes)
                .map_err(de_err("cn_reservation"))?;
            out.push(v);
        }
        out.sort_by_key(|r| (r.server_uuid, r.saga_id));
        Ok(out)
    }

    async fn put_cn_load_summary(&self, row: CnLoadSummary) -> Result<(), StoreError> {
        let key = keys::cn_load_summary_key(row.server_uuid);
        let value = serde_json::to_vec(&row)
            .map_err(ser_err("cn_load_summary"))?;
        self.db
            .run(|tr, _| {
                let key = key.clone();
                let value = value.clone();
                async move {
                    tr.set(&key, &value);
                    Ok(())
                }
            })
            .await
            .map_err(StoreError::from)
    }

    async fn get_cn_load_summary(
        &self,
        server_uuid: Uuid,
    ) -> Result<Option<CnLoadSummary>, StoreError> {
        let key = keys::cn_load_summary_key(server_uuid);
        let bytes: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;
        match bytes.map_err(StoreError::from)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(de_err("cn_load_summary"))?)),
            None => Ok(None),
        }
    }

    async fn list_cn_load_summaries(&self) -> Result<Vec<CnLoadSummary>, StoreError> {
        let prefix = keys::cn_load_summary_prefix().to_vec();
        let (begin, end) = prefix_range(&prefix);
        let bytes_list: Result<Vec<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        mode: StreamingMode::WantAll,
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok(kvs.iter().map(|kv| kv.value().to_vec()).collect())
                }
            })
            .await;
        let bytes_list =
            bytes_list.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(bytes_list.len());
        for bytes in bytes_list {
            let v: CnLoadSummary = serde_json::from_slice(&bytes)
                .map_err(de_err("cn_load_summary"))?;
            out.push(v);
        }
        out.sort_by_key(|s| s.server_uuid);
        Ok(out)
    }

    async fn put_instance_affinity(&self, row: InstanceAffinity) -> Result<(), StoreError> {
        let by_id_key = keys::instance_affinity_key(row.instance_id);
        let by_tenant_key = keys::instance_affinity_by_tenant_key(row.tenant_uuid, row.instance_id);
        let value = serde_json::to_vec(&row)
            .map_err(ser_err("instance_affinity"))?;
        // If the row's tenant changed across edits we'd leak the old
        // by_tenant index entry; for v1 the tenant is fixed at create
        // time and never re-parented, so we don't pay for the read
        // here. PL-7's editing surface adds the read-and-clean-up
        // path when (and if) it allows tenant reassignment.
        self.db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_tenant_key = by_tenant_key.clone();
                let value = value.clone();
                async move {
                    tr.set(&by_id_key, &value);
                    tr.set(&by_tenant_key, b"");
                    Ok(())
                }
            })
            .await
            .map_err(StoreError::from)
    }

    async fn get_instance_affinity(
        &self,
        instance_id: Uuid,
    ) -> Result<InstanceAffinity, StoreError> {
        let key = keys::instance_affinity_key(instance_id);
        let bytes: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;
        let bytes = bytes
            .map_err(StoreError::from)?
            .ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("instance_affinity"))
    }

    async fn list_instance_affinities_for_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<InstanceAffinity>, StoreError> {
        let prefix = keys::instance_affinity_by_tenant_prefix(tenant_id);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        // First pass: collect the instance_ids from the by_tenant
        // membership index. Second pass: range-read the actual
        // affinity rows in one FDB transaction.
        let ids: Result<Vec<Uuid>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        mode: StreamingMode::WantAll,
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    let mut out = Vec::with_capacity(kvs.len());
                    for kv in kvs.iter() {
                        let suffix = &kv.key()[prefix_len..];
                        let s = std::str::from_utf8(suffix).map_err(txn_err("instance_affinity_by_tenant key utf-8"))?;
                        let id = Uuid::parse_str(s).map_err(txn_err("parse instance_id uuid"))?;
                        out.push(id);
                    }
                    Ok(out)
                }
            })
            .await;
        let ids = ids.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            // Reads happen outside the index-scan transaction; PL-2
            // doesn't need a single-snapshot read for the scoring
            // path (the chain runner builds its CnView for the
            // candidate CN itself). PL-5's `designate` action wraps
            // the full snapshot in one transaction.
            let row = self.get_instance_affinity(id).await?;
            out.push(row);
        }
        out.sort_by_key(|r| r.instance_id);
        Ok(out)
    }

    // ---- Joined snapshots for the placement engine (PL-5) ----

    async fn get_cn_pick_snapshot(&self, server_uuid: Uuid) -> Result<CnPickSnapshot, StoreError> {
        // PL-5a: compose existing per-keyspace reads. The
        // single-FDB-txn shape the saga action needs is encoded
        // in the saga action's body itself at PL-5b: it wraps
        // get_cn_pick_snapshot + reserve_cn_capacity +
        // set_instance_host_cn inside one transaction so the
        // capacity-residual check and the reservation write
        // share a read version.
        let cn = self.get_cn(server_uuid).await?;
        let capacity = match self.get_cn_capacity(server_uuid).await {
            Ok(c) => Some(c),
            Err(StoreError::NotFound) => None,
            Err(e) => return Err(e),
        };
        let placement = self.get_cn_placement(server_uuid).await?;
        let reservations = self.list_cn_reservations(Some(server_uuid)).await?;
        let load_summary = self.get_cn_load_summary(server_uuid).await?;
        let assigned_instances = self.list_instances_for_cn(server_uuid).await?;
        Ok(CnPickSnapshot {
            cn,
            capacity,
            placement,
            reservations,
            load_summary,
            assigned_instances,
            computed_at: Utc::now(),
        })
    }

    async fn list_tenant_instance_projections(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<TenantInstanceProjection>, StoreError> {
        // Compose: list every CnPlacement so we can index
        // (cn_uuid -> fault_domain), then range-scan instances
        // and filter by tenant. PL-5a treats this as a non-
        // single-transaction read for the same reason as
        // `get_cn_pick_snapshot`.
        let placements = self.list_cn_placements().await?;
        let fault_domains: std::collections::HashMap<Uuid, Option<String>> = placements
            .into_iter()
            .map(|p| (p.server_uuid, p.fault_domain))
            .collect();
        // No instance-by-tenant index in v1; walk projects, then
        // per-project listings. A future slice can add the index
        // if this becomes hot.
        let projects = self.list_projects_in_tenant(tenant_id).await?;
        let mut instances: Vec<Instance> = Vec::new();
        for p in projects {
            instances.extend(self.list_instances_in_project(p.id).await?);
        }
        Ok(instances
            .into_iter()
            .map(|i| {
                let host_fault_domain = i
                    .host_cn_uuid
                    .and_then(|cn| fault_domains.get(&cn).cloned().flatten());
                TenantInstanceProjection {
                    instance: i,
                    host_fault_domain,
                }
            })
            .collect())
    }

    async fn upsert_legacy_vm(&self, legacy_vm: LegacyVm) -> Result<(), StoreError> {
        let smartos_uuid = legacy_vm.smartos_uuid;
        let new_host = legacy_vm.host_cn_uuid;
        let by_id_key = keys::legacy_vm_by_id_key(smartos_uuid);
        let instance_key = keys::instance_by_id_key(smartos_uuid);
        let new_membership_key = keys::legacy_vm_in_host_cn_key(new_host, smartos_uuid);
        let value = serde_json::to_vec(&legacy_vm)
            .map_err(ser_err("legacy_vm"))?;

        // Sentinel for the UUID-uniqueness check; outer match
        // converts to a typed StoreError without leaking FDB.
        enum Outcome {
            Ok,
            ConflictWithInstance,
        }

        let result: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let instance_key = instance_key.clone();
                let new_membership_key = new_membership_key.clone();
                let value = value.clone();
                async move {
                    // UUID-uniqueness invariant: a SmartOS zone uuid
                    // can be EITHER a tritond-managed Instance OR a
                    // LegacyVm, never both. The classifier prevents
                    // this at the upstream (Managed-fallback path),
                    // but we enforce here too as defense-in-depth so
                    // an admin import script -- or a future Phase D
                    // adoption flow racing the discovery loop --
                    // can't create a duplicate inside the same txn
                    // that would otherwise succeed.
                    if tr.get(&instance_key, false).await?.is_some() {
                        return Ok(Outcome::ConflictWithInstance);
                    }
                    // If a LegacyVm record already exists with a
                    // different host, drop the old membership-index
                    // entry inside the same txn so the move is atomic.
                    if let Some(existing_bytes) = tr.get(&by_id_key, false).await? {
                        let existing: LegacyVm =
                            serde_json::from_slice(&existing_bytes).map_err(txn_de_err("legacy_vm"))?;
                        if existing.host_cn_uuid != new_host {
                            let old_membership_key =
                                keys::legacy_vm_in_host_cn_key(existing.host_cn_uuid, smartos_uuid);
                            tr.clear(&old_membership_key);
                        }
                    }
                    tr.set(&by_id_key, &value);
                    tr.set(&new_membership_key, b"");
                    Ok(Outcome::Ok)
                }
            })
            .await;
        match result {
            Ok(Outcome::Ok) => Ok(()),
            Ok(Outcome::ConflictWithInstance) => Err(StoreError::Conflict(format!(
                "smartos_uuid {smartos_uuid} already exists as a managed Instance",
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_legacy_vm(&self, smartos_uuid: Uuid) -> Result<LegacyVm, StoreError> {
        let key = keys::legacy_vm_by_id_key(smartos_uuid);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("legacy_vm"))
    }

    async fn list_legacy_vms(&self) -> Result<Vec<LegacyVm>, StoreError> {
        let prefix = keys::legacy_vm_by_id_prefix().to_vec();
        let (begin, end) = prefix_range(&prefix);
        let bytes_list: Result<Vec<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    // `WantAll` returns the full range in one shot
                    // (subject to FDB's 5MB transaction size cap).
                    // The default `Iterator` mode returns only the
                    // first chunk (~80KB / ~100 rows) on a single
                    // get_range call, which truncated discovery
                    // results once the fleet had >25 legacy zones.
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        mode: StreamingMode::WantAll,
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    let mut out = Vec::new();
                    for kv in kvs.iter() {
                        out.push(kv.value().to_vec());
                    }
                    Ok(out)
                }
            })
            .await;
        let bytes_list =
            bytes_list.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(bytes_list.len());
        for bytes in bytes_list {
            let v: LegacyVm = serde_json::from_slice(&bytes)
                .map_err(de_err("legacy_vm"))?;
            out.push(v);
        }
        out.sort_by_key(|v| v.smartos_uuid);
        Ok(out)
    }

    async fn list_legacy_vms_for_cn(
        &self,
        host_cn_uuid: Uuid,
    ) -> Result<Vec<LegacyVm>, StoreError> {
        let prefix = keys::legacy_vm_in_host_cn_prefix(host_cn_uuid);
        let (begin, end) = prefix_range(&prefix);
        let prefix_len = prefix.len();

        let id_strs: Result<Vec<String>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin.clone();
                let end = end.clone();
                async move {
                    // See list_legacy_vms note on StreamingMode.
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        mode: StreamingMode::WantAll,
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
        let id_strs = id_strs.map_err(StoreError::from)?;
        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("legacy_vm host index uuid: {e}")))?;
            let by_id_key = keys::legacy_vm_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let v: LegacyVm = serde_json::from_slice(&bytes)
                    .map_err(de_err("legacy_vm"))?;
                out.push(v);
            }
        }
        out.sort_by_key(|v| v.smartos_uuid);
        Ok(out)
    }

    async fn delete_legacy_vm(&self, smartos_uuid: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::legacy_vm_by_id_key(smartos_uuid);
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    if let Some(bytes) = tr.get(&by_id_key, false).await? {
                        let existing: LegacyVm = serde_json::from_slice(&bytes).map_err(txn_de_err("legacy_vm"))?;
                        let membership_key =
                            keys::legacy_vm_in_host_cn_key(existing.host_cn_uuid, smartos_uuid);
                        tr.clear(&membership_key);
                        tr.clear(&by_id_key);
                    }
                    // Idempotent: missing record is not an error.
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    async fn get_auto_approve_window(&self) -> Result<Option<AutoApproveWindow>, StoreError> {
        let key = keys::auto_approve_window_key().to_vec();
        let bytes = self.read_bytes(&key).await?;
        match bytes {
            Some(bytes) => {
                let w: AutoApproveWindow = serde_json::from_slice(&bytes).map_err(de_err("auto-approve window"))?;
                Ok(Some(w))
            }
            None => Ok(None),
        }
    }

    async fn open_auto_approve_window(&self, w: AutoApproveWindow) -> Result<(), StoreError> {
        let value = serde_json::to_vec(&w)
            .map_err(ser_err("auto-approve window"))?;
        let key = keys::auto_approve_window_key().to_vec();
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
        result.map_err(StoreError::from)
    }

    async fn close_auto_approve_window(&self) -> Result<(), StoreError> {
        let key = keys::auto_approve_window_key().to_vec();
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
        result.map_err(StoreError::from)
    }

    async fn try_consume_auto_approve_slot(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let key = keys::auto_approve_window_key().to_vec();
        let result: Result<bool, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { consume_auto_approve_slot_in_txn(&tr, &key, now).await }
            })
            .await;
        result.map_err(StoreError::from)
    }

    // ------------------------------------------------------------------
    // Realized network state (Slice H-1)
    // ------------------------------------------------------------------

    async fn record_network_realization(
        &self,
        resource: NetworkResourceId,
        realizer: RealizerId,
        generation: u64,
        status: RealizationStatus,
        message: Option<String>,
    ) -> Result<(), StoreError> {
        let key = keys::network_realization_key(resource, realizer);

        enum Outcome {
            Stored,
            Backward { existing: u64 },
            SerializeFailed,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let message = message.clone();
                async move {
                    if let Some(bytes) = tr.get(&key, false).await?
                        && let Ok(existing) = serde_json::from_slice::<Realization>(&bytes)
                        && existing.generation > generation
                    {
                        return Ok(Outcome::Backward {
                            existing: existing.generation,
                        });
                    }
                    let row = Realization {
                        realizer,
                        generation,
                        status,
                        last_reported_at: Utc::now(),
                        message,
                    };
                    let value = match serde_json::to_vec(&row) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::SerializeFailed),
                    };
                    tr.set(&key, &value);
                    Ok(Outcome::Stored)
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Stored) => Ok(()),
            Ok(Outcome::Backward { existing }) => Err(StoreError::Conflict(format!(
                "backward generation report for {} {}: existing={existing}, attempted={generation}",
                resource.kind_tag(),
                resource.id(),
            ))),
            Ok(Outcome::SerializeFailed) => {
                Err(StoreError::Backend("serialize realization row".to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn list_network_realizations(
        &self,
        resource: NetworkResourceId,
    ) -> Result<Vec<Realization>, StoreError> {
        let prefix = keys::network_realization_resource_prefix(resource);
        let (begin, end) = prefix_range(&prefix);

        let result: Result<Vec<Realization>, FdbBindingError> = self
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
                    let mut rows: Vec<Realization> = Vec::with_capacity(kvs.len());
                    for kv in kvs.iter() {
                        if let Ok(row) = serde_json::from_slice::<Realization>(kv.value()) {
                            rows.push(row);
                        }
                    }
                    Ok(rows)
                }
            })
            .await;

        let mut rows = result.map_err(StoreError::from)?;
        rows.sort_by(|a, b| {
            a.realizer
                .kind_tag()
                .cmp(b.realizer.kind_tag())
                .then_with(|| a.realizer.id().cmp(&b.realizer.id()))
        });
        Ok(rows)
    }

    // ----- Firewall rules (Slice 1): not yet implemented in FDB ------
    //
    // Slice 1 lands the per-VPC firewall rule API on top of the
    // in-memory backend so the tritond → proteus blueprint pipeline
    // can be exercised end-to-end without depending on FDB. The FDB
    // keyspace + transactional CRUD lands as a follow-up; the trait
    // forces these methods to exist so service handlers compile in
    // both feature combinations, but they all surface the same
    // explicit not-yet-implemented backend error so any production
    // tritond accidentally calling them gets a clear signal rather
    // than wrong-data corruption.

    async fn list_silos(&self) -> Result<Vec<Silo>, StoreError> {
        // Range-scan `silo/by_id/` and decode each value (silos store
        // their full JSON in the by_id key, no separate index lookup).
        let prefix = b"silo/by_id/".to_vec();
        let (begin, end) = prefix_range(&prefix);

        let kvs: Result<Vec<Vec<u8>>, FdbBindingError> = self
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
        let kvs = kvs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(kvs.len());
        for bytes in kvs {
            let silo: Silo = serde_json::from_slice(&bytes)
                .map_err(de_err("silo"))?;
            out.push(silo);
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn create_firewall_rule(
        &self,
        _tenant_id: Uuid,
        _project_id: Uuid,
        _vpc_id: Uuid,
        _req: NewFirewallRule,
    ) -> Result<FirewallRule, StoreError> {
        validate::name("firewall_rule", &req.name)?;
        Err(firewall_rules_not_in_fdb_yet())
    }

    async fn get_firewall_rule(&self, _rule_id: Uuid) -> Result<FirewallRule, StoreError> {
        Err(firewall_rules_not_in_fdb_yet())
    }

    async fn list_firewall_rules_in_vpc(
        &self,
        _vpc_id: Uuid,
    ) -> Result<Vec<FirewallRule>, StoreError> {
        // Empty list rather than an error so an FDB-backed tritond's
        // build_port_blueprint call still succeeds (with the dataplane
        // baseline-only firewall behaviour) until the FDB CRUD lands.
        Ok(Vec::new())
    }

    async fn delete_firewall_rule(&self, _rule_id: Uuid) -> Result<(), StoreError> {
        Err(firewall_rules_not_in_fdb_yet())
    }

    // ------------------------------------------------------------------
    // DHCP / IPAM (γ.1, γ.4, γ.3)
    //
    // Schema: see the module-level doc-comment at the top of this file.
    // Every multi-key write happens inside a single transaction so name
    // uniqueness, cidr containment, and existence-of-parent checks are
    // enforced atomically. Each method mirrors the in-memory store's
    // semantics so a deployment that switches backends sees the same
    // wire behaviour.
    // ------------------------------------------------------------------

    async fn get_dhcp_pool(&self, vpc_id: Uuid) -> Result<Option<DhcpPool>, StoreError> {
        let key = keys::dhcp_pool_by_vpc_key(vpc_id);
        match self.read_bytes(&key).await? {
            None => Ok(None),
            Some(bytes) => {
                let pool: DhcpPool = serde_json::from_slice(&bytes)
                    .map_err(de_err("dhcp pool"))?;
                Ok(Some(pool))
            }
        }
    }

    async fn set_dhcp_pool(&self, vpc_id: Uuid, req: NewDhcpPool) -> Result<DhcpPool, StoreError> {
        let vpc_key = keys::vpc_by_id_key(vpc_id);
        let pool_key = keys::dhcp_pool_by_vpc_key(vpc_id);

        enum Outcome {
            Stored(Box<DhcpPool>),
            VpcMissing,
        }

        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let vpc_key = vpc_key.clone();
                let pool_key = pool_key.clone();
                let req = req.clone();
                async move {
                    if tr.get(&vpc_key, false).await?.is_none() {
                        return Ok(Outcome::VpcMissing);
                    }
                    let now = Utc::now();
                    let created_at = match tr.get(&pool_key, false).await? {
                        Some(b) => match serde_json::from_slice::<DhcpPool>(&b) {
                            Ok(existing) => existing.created_at,
                            Err(_) => now,
                        },
                        None => now,
                    };
                    let pool = DhcpPool {
                        vpc_id,
                        lease_seconds_default: req.lease_seconds_default,
                        excluded_ipv4: req.excluded_ipv4,
                        additional_options: req.additional_options,
                        created_at,
                        updated_at: now,
                    };
                    let value = serde_json::to_vec(&pool).map_err(txn_ser_err("dhcp pool"))?;
                    tr.set(&pool_key, &value);
                    Ok(Outcome::Stored(Box::new(pool)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Stored(pool)) => Ok(*pool),
            Ok(Outcome::VpcMissing) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn clear_dhcp_pool(&self, vpc_id: Uuid) -> Result<(), StoreError> {
        let pool_key = keys::dhcp_pool_by_vpc_key(vpc_id);

        enum Out {
            Cleared,
            Missing,
        }

        let outcome: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let pool_key = pool_key.clone();
                async move {
                    if tr.get(&pool_key, false).await?.is_none() {
                        return Ok(Out::Missing);
                    }
                    tr.clear(&pool_key);
                    Ok(Out::Cleared)
                }
            })
            .await;

        match outcome {
            Ok(Out::Cleared) => Ok(()),
            Ok(Out::Missing) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_dhcp_reservations(
        &self,
        vpc_id: Uuid,
    ) -> Result<Vec<DhcpReservation>, StoreError> {
        let prefix = keys::dhcp_reservation_by_vpc_prefix(vpc_id);
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
            let reservation: DhcpReservation = serde_json::from_slice(&bytes)
                .map_err(de_err("dhcp reservation"))?;
            out.push(reservation);
        }
        out.sort_by(|a, b| a.mac.cmp(&b.mac));
        Ok(out)
    }

    async fn create_dhcp_reservation(
        &self,
        vpc_id: Uuid,
        req: NewDhcpReservation,
    ) -> Result<DhcpReservation, StoreError> {
        let mac = crate::types::canonical_mac(&req.mac)?;
        let vpc_key = keys::vpc_by_id_key(vpc_id);
        let res_key = keys::dhcp_reservation_by_vpc_mac_key(vpc_id, &mac);

        enum Outcome {
            Created(Box<DhcpReservation>),
            VpcMissing,
            OutsideCidr,
            MacAlreadyReservedDifferent { existing_ipv4: std::net::Ipv4Addr },
        }

        let mac_for_txn = mac.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let vpc_key = vpc_key.clone();
                let res_key = res_key.clone();
                let req = req.clone();
                let mac = mac_for_txn.clone();
                async move {
                    let vpc_bytes = match tr.get(&vpc_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Outcome::VpcMissing),
                    };
                    let vpc: Vpc = match serde_json::from_slice(&vpc_bytes) {
                        Ok(v) => v,
                        Err(_) => return Ok(Outcome::VpcMissing),
                    };
                    if let Some(cidr) = vpc.ipv4_block
                        && !crate::types::cidr_contains_ipv4(IpNetwork::V4(cidr), req.ipv4)
                    {
                        return Ok(Outcome::OutsideCidr);
                    }
                    if let Some(b) = tr.get(&res_key, false).await?
                        && let Ok(existing) = serde_json::from_slice::<DhcpReservation>(&b)
                        && existing.ipv4 != req.ipv4
                    {
                        return Ok(Outcome::MacAlreadyReservedDifferent {
                            existing_ipv4: existing.ipv4,
                        });
                    }
                    let now = Utc::now();
                    let reservation = DhcpReservation {
                        vpc_id,
                        mac,
                        ipv4: req.ipv4,
                        hostname: req.hostname,
                        per_mac_options: req.per_mac_options,
                        created_at: now,
                    };
                    let value = serde_json::to_vec(&reservation).map_err(txn_ser_err("dhcp reservation"))?;
                    tr.set(&res_key, &value);
                    Ok(Outcome::Created(Box::new(reservation)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(r)) => Ok(*r),
            Ok(Outcome::VpcMissing) => Err(StoreError::NotFound),
            Ok(Outcome::OutsideCidr) => Err(StoreError::Conflict(format!(
                "reservation ipv4 {} is outside vpc ipv4 block",
                req.ipv4
            ))),
            Ok(Outcome::MacAlreadyReservedDifferent { existing_ipv4 }) => {
                Err(StoreError::Conflict(format!(
                    "mac {mac} already reserved with a different ipv4 ({existing_ipv4}); delete first"
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn get_dhcp_reservation(
        &self,
        vpc_id: Uuid,
        mac: &str,
    ) -> Result<DhcpReservation, StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let key = keys::dhcp_reservation_by_vpc_mac_key(vpc_id, &mac);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("dhcp reservation"))
    }

    async fn delete_dhcp_reservation(&self, vpc_id: Uuid, mac: &str) -> Result<(), StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let key = keys::dhcp_reservation_by_vpc_mac_key(vpc_id, &mac);

        enum Out {
            Deleted,
            Missing,
        }
        let outcome: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move {
                    if tr.get(&key, false).await?.is_none() {
                        return Ok(Out::Missing);
                    }
                    tr.clear(&key);
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) => Ok(()),
            Ok(Out::Missing) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_dhcp_leases(&self, vpc_id: Uuid) -> Result<Vec<DhcpLease>, StoreError> {
        let prefix = keys::dhcp_lease_by_vpc_prefix(vpc_id);
        self.scan_dhcp_leases(prefix).await.map(|mut v| {
            v.sort_by(|a, b| a.created_at.cmp(&b.created_at));
            v
        })
    }

    async fn get_dhcp_lease(&self, vpc_id: Uuid, mac: &str) -> Result<DhcpLease, StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let key = keys::dhcp_lease_by_vpc_mac_key(vpc_id, &mac);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("dhcp lease"))
    }

    async fn record_dhcp_lease(&self, mut lease: DhcpLease) -> Result<DhcpLease, StoreError> {
        lease.mac = crate::types::canonical_mac(&lease.mac)?;
        let key = keys::dhcp_lease_by_vpc_mac_key(lease.vpc_id, &lease.mac);
        let value = serde_json::to_vec(&lease)
            .map_err(ser_err("dhcp lease"))?;
        // RFD 00007 AP-1c: MAC -> vpc index. Stored as the canonical
        // lease key components so a reader can resolve a bare MAC to
        // its parent VPC without scanning every VPC's lease prefix.
        let by_mac_key = keys::dhcp_lease_by_mac_key(&lease.mac);
        let vpc_str = lease.vpc_id.to_string();

        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let value = value.clone();
                let by_mac_key = by_mac_key.clone();
                let vpc_str = vpc_str.clone();
                async move {
                    tr.set(&key, &value);
                    tr.set(&by_mac_key, vpc_str.as_bytes());
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)?;
        Ok(lease)
    }

    async fn delete_dhcp_lease(&self, vpc_id: Uuid, mac: &str) -> Result<(), StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let key = keys::dhcp_lease_by_vpc_mac_key(vpc_id, &mac);
        let by_mac_key = keys::dhcp_lease_by_mac_key(&mac);

        enum Out {
            Deleted,
            Missing,
        }
        let outcome: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let by_mac_key = by_mac_key.clone();
                let vpc_str = vpc_id.to_string();
                async move {
                    if tr.get(&key, false).await?.is_none() {
                        return Ok(Out::Missing);
                    }
                    tr.clear(&key);
                    // RFD 00007 AP-1c: drop the MAC index entry only
                    // if it still points at this VPC (a concurrent
                    // re-issue against a different VPC would have
                    // overwritten the index; checking the value first
                    // keeps the index honest).
                    if let Some(current) = tr.get(&by_mac_key, false).await?
                        && current.as_ref() == vpc_str.as_bytes()
                    {
                        tr.clear(&by_mac_key);
                    }
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) => Ok(()),
            Ok(Out::Missing) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_all_dhcp_leases(&self) -> Result<Vec<DhcpLease>, StoreError> {
        let prefix = keys::dhcp_lease_global_prefix().to_vec();
        self.scan_dhcp_leases(prefix).await.map(|mut v| {
            v.sort_by(|a, b| {
                a.vpc_id
                    .cmp(&b.vpc_id)
                    .then(a.created_at.cmp(&b.created_at))
            });
            v
        })
    }

    // ------------------------------------------------------------------
    // Storage clusters (operator-only)
    // ------------------------------------------------------------------

    async fn create_storage_cluster(
        &self,
        req: NewStorageCluster,
    ) -> Result<StorageCluster, StoreError> {
        validate::name("storage_cluster", &req.name)?;
        let id = Uuid::new_v4();
        let by_id_key = keys::storage_cluster_by_id_key(id);
        let by_name_key = keys::storage_cluster_by_name_key(&req.name);
        let all_key = keys::storage_cluster_all_key(id);
        let id_bytes = id.to_string().into_bytes();

        enum Outcome {
            Created(Box<StorageCluster>),
            NameTaken,
            SerializeFailed(String),
        }

        let req_for_txn = req.clone();
        let outcome: Result<Outcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let all_key = all_key.clone();
                let id_bytes = id_bytes.clone();
                let req = req_for_txn.clone();
                async move {
                    if tr.get(&by_name_key, false).await?.is_some() {
                        return Ok(Outcome::NameTaken);
                    }

                    let cluster = StorageCluster {
                        id,
                        name: req.name.clone(),
                        surface: req.surface,
                        endpoint: req.endpoint.clone(),
                        admin_token: req.admin_token.clone(),
                        default_region: req.default_region.clone(),
                        display_name: req.display_name.clone(),
                        status: StorageClusterStatus::Unknown,
                        created_at: Utc::now(),
                        last_observed_at: None,
                        s3_endpoint: None,
                        presigner_access_key_id: None,
                        presigner_secret_access_key: None,
                    };
                    let value = match serde_json::to_vec(&cluster) {
                        Ok(v) => v,
                        Err(e) => return Ok(Outcome::SerializeFailed(e.to_string())),
                    };
                    tr.set(&by_id_key, &value);
                    tr.set(&by_name_key, &id_bytes);
                    tr.set(&all_key, b"");
                    Ok(Outcome::Created(Box::new(cluster)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Created(c)) => Ok(*c),
            Ok(Outcome::NameTaken) => Err(StoreError::Conflict(format!(
                "storage cluster with name {:?} already exists",
                req.name
            ))),
            Ok(Outcome::SerializeFailed(e)) => Err(StoreError::Backend(format!(
                "serialize storage cluster: {e}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_storage_cluster(&self, id: Uuid) -> Result<StorageCluster, StoreError> {
        let key = keys::storage_cluster_by_id_key(id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(de_err("storage cluster"))
    }

    async fn get_storage_cluster_by_name(&self, name: &str) -> Result<StorageCluster, StoreError> {
        let by_name_key = keys::storage_cluster_by_name_key(name);
        let id_bytes = self
            .read_bytes(&by_name_key)
            .await?
            .ok_or(StoreError::NotFound)?;
        let id_str = std::str::from_utf8(&id_bytes)
            .map_err(|e| StoreError::Backend(format!("storage cluster name index utf8: {e}")))?;
        let id = Uuid::parse_str(id_str)
            .map_err(|e| StoreError::Backend(format!("storage cluster name index uuid: {e}")))?;
        self.get_storage_cluster(id).await
    }

    async fn list_storage_clusters(&self) -> Result<Vec<StorageCluster>, StoreError> {
        let prefix = keys::storage_cluster_all_prefix().to_vec();
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
            .await;
        let id_strs = id_strs.map_err(StoreError::from)?;

        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("storage cluster index uuid: {e}")))?;
            match self.get_storage_cluster(id).await {
                Ok(cluster) => out.push(cluster),
                Err(StoreError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn delete_storage_cluster(&self, id: Uuid) -> Result<(), StoreError> {
        let by_id_key = keys::storage_cluster_by_id_key(id);

        enum Out {
            Deleted,
            Vanished,
            Corrupt(String),
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
                    let cluster: StorageCluster = match serde_json::from_slice(&bytes) {
                        Ok(c) => c,
                        Err(e) => return Ok(Out::Corrupt(e.to_string())),
                    };
                    tr.clear(&by_id_key);
                    tr.clear(&keys::storage_cluster_by_name_key(&cluster.name));
                    tr.clear(&keys::storage_cluster_all_key(cluster.id));
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) | Ok(Out::Vanished) => Ok(()),
            Ok(Out::Corrupt(e)) => Err(StoreError::Backend(format!(
                "deserialize storage cluster: {e}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn update_storage_cluster_status(
        &self,
        id: Uuid,
        status: StorageClusterStatus,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<StorageCluster, StoreError> {
        let by_id_key = keys::storage_cluster_by_id_key(id);

        enum Out {
            Updated(Box<StorageCluster>),
            Vanished,
            Corrupt(String),
            SerializeFailed(String),
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
                    let mut cluster: StorageCluster = match serde_json::from_slice(&bytes) {
                        Ok(c) => c,
                        Err(e) => return Ok(Out::Corrupt(e.to_string())),
                    };
                    cluster.status = status;
                    cluster.last_observed_at = Some(observed_at);
                    let value = match serde_json::to_vec(&cluster) {
                        Ok(v) => v,
                        Err(e) => return Ok(Out::SerializeFailed(e.to_string())),
                    };
                    tr.set(&by_id_key, &value);
                    Ok(Out::Updated(Box::new(cluster)))
                }
            })
            .await;

        match outcome {
            Ok(Out::Updated(c)) => Ok(*c),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::Corrupt(e)) => Err(StoreError::Backend(format!(
                "deserialize storage cluster: {e}"
            ))),
            Ok(Out::SerializeFailed(e)) => Err(StoreError::Backend(format!(
                "serialize storage cluster: {e}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    async fn update_storage_cluster_presigner(
        &self,
        id: Uuid,
        s3_endpoint: Option<String>,
        access_key_id: Option<String>,
        secret_access_key: Option<String>,
    ) -> Result<StorageCluster, StoreError> {
        match (&access_key_id, &secret_access_key) {
            (Some(_), Some(_)) | (None, None) => {}
            _ => {
                return Err(StoreError::Conflict(
                    "presigner credentials must be set or cleared together".into(),
                ));
            }
        }
        let by_id_key = keys::storage_cluster_by_id_key(id);

        enum Out {
            Updated(Box<StorageCluster>),
            Vanished,
            Corrupt(String),
            SerializeFailed(String),
        }

        let outcome: Result<Out, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let s3_endpoint = s3_endpoint.clone();
                let access_key_id = access_key_id.clone();
                let secret_access_key = secret_access_key.clone();
                async move {
                    let bytes = match tr.get(&by_id_key, false).await? {
                        Some(b) => b,
                        None => return Ok(Out::Vanished),
                    };
                    let mut cluster: StorageCluster = match serde_json::from_slice(&bytes) {
                        Ok(c) => c,
                        Err(e) => return Ok(Out::Corrupt(e.to_string())),
                    };
                    if let Some(ep) = s3_endpoint {
                        cluster.s3_endpoint = Some(ep);
                    }
                    cluster.presigner_access_key_id = access_key_id;
                    cluster.presigner_secret_access_key = secret_access_key;
                    let value = match serde_json::to_vec(&cluster) {
                        Ok(v) => v,
                        Err(e) => return Ok(Out::SerializeFailed(e.to_string())),
                    };
                    tr.set(&by_id_key, &value);
                    Ok(Out::Updated(Box::new(cluster)))
                }
            })
            .await;

        match outcome {
            Ok(Out::Updated(c)) => Ok(*c),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::Corrupt(e)) => Err(StoreError::Backend(format!(
                "deserialize storage cluster: {e}"
            ))),
            Ok(Out::SerializeFailed(e)) => Err(StoreError::Backend(format!(
                "serialize storage cluster: {e}"
            ))),
            Err(e) => Err(e.into()),
        }
    }
}

fn firewall_rules_not_in_fdb_yet() -> StoreError {
    StoreError::Backend(
        "firewall rule CRUD is not yet implemented in the FoundationDB backend (Slice 1 ships \
         only on the in-memory store; the FDB keyspace + transactional CRUD is the immediate \
         follow-up)"
            .into(),
    )
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
    keys::subnet_in_vpc_prefix(vpc_id).len()
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
        let key = keys::cn_by_claim_key(code);
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
        let key = keys::cn_by_poll_key(token);
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
    let mut window: AutoApproveWindow = serde_json::from_slice(&bytes).map_err(txn_de_err("auto-approve window"))?;
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
                let value = serde_json::to_vec(&window).map_err(txn_ser_err("auto-approve window"))?;
                tr.set(window_key, &value);
            }
            Ok(true)
        }
        None => Ok(true),
    }
}


#[cfg(test)]
mod cn_tests;

#[cfg(test)]
mod route_target_tests;

#[cfg(test)]
mod network_realization_tests;

#[cfg(test)]
mod tenant_tests;
