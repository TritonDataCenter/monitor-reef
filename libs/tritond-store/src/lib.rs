// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Persistence layer for the Triton Cloud control plane.
//!
//! This crate defines the domain types and the [`Store`] trait that
//! `tritond` uses to read and write its state. Two backends ship in
//! this crate:
//!
//! * [`MemStore`] — an in-process `RwLock<HashMap>` implementation,
//!   used for tests and for `tritond` runs that don't need durable
//!   state. Always available.
//! * `FdbStore` (behind the `foundationdb` cargo feature, added in a
//!   subsequent commit) — the production FoundationDB-backed
//!   implementation.
//!
//! The trait deliberately deals only in plain Rust types. It does not
//! know about HTTP, JSON, or Dropshot; the wire surface lives in
//! `tritond-api` and re-uses the types defined here so there is no
//! API↔storage conversion layer to keep in sync.

#[cfg(feature = "foundationdb")]
pub mod fdb;
pub mod mem;
mod types;

#[cfg(feature = "foundationdb")]
pub use fdb::FdbStore;
pub use mem::MemStore;
pub use types::{
    AddressFamily, ApiKey, ApiKeyScope, ApiKeyView, Disk, DiskKind, FLOATING_IP_V4_POOL,
    FLOATING_IP_V6_POOL, Federation, FloatingIp, FloatingIpAttachment, IdpConfig, IdpConfigView,
    Image, Instance, InstanceCreateResult, JobKind, JobOutcome, JobStatus, JobStatusKind,
    LifecycleState, LifecycleStateKind, NewFloatingIp, NewImage, NewInstance, NewJob, NewProject,
    NewQuota, NewSilo, NewSshKey, NewSubnet, NewVpc, Nic, Project, ProvisioningJob, Quota, Silo,
    SshKey, Subnet, SystemKey, TRITOND_IMAGE_NAMESPACE, User, UserView, VPC_VNI_MAX,
    VPC_VNI_RESERVED_CEILING, Vpc, derive_image_id,
};

use async_trait::async_trait;
use uuid::Uuid;

/// Errors that a [`Store`] implementation may produce.
///
/// Phase 0 surfaces only the conditions that are meaningful to API
/// callers (not-found, name conflicts) plus a catch-all `Backend`
/// variant for transport / driver failures. Once `FdbStore` lands the
/// catch-all gains FoundationDB-specific context.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The requested entity does not exist.
    #[error("not found")]
    NotFound,

    /// A unique constraint was violated (e.g. silo name already taken).
    #[error("conflict: {0}")]
    Conflict(String),

    /// The backing store reported an error that the caller cannot meaningfully
    /// react to beyond surfacing it.
    #[error("store backend error: {0}")]
    Backend(String),
}

/// A handle to the control-plane state store.
///
/// All methods are async because the production backend
/// (FoundationDB) is async; the in-memory implementation simulates
/// async by being trivially `await`-able.
#[async_trait]
pub trait Store: Send + Sync + 'static {
    // ------------------------------------------------------------------
    // Silos
    // ------------------------------------------------------------------

    /// Create a new silo.
    ///
    /// Returns [`StoreError::Conflict`] if `req.name` is already in use.
    async fn create_silo(&self, req: NewSilo) -> Result<Silo, StoreError>;

    /// Look up a silo by id.
    ///
    /// Returns [`StoreError::NotFound`] if no silo with that id exists.
    async fn get_silo(&self, id: Uuid) -> Result<Silo, StoreError>;

    // ------------------------------------------------------------------
    // Users
    // ------------------------------------------------------------------

    /// Create a new operator account.
    ///
    /// Returns [`StoreError::Conflict`] if `user.username` is already
    /// in use.
    async fn create_user(&self, user: User) -> Result<User, StoreError>;

    /// Look up an operator by username.
    async fn get_user_by_username(&self, username: &str) -> Result<User, StoreError>;

    /// Look up an operator by id.
    async fn get_user_by_id(&self, id: Uuid) -> Result<User, StoreError>;

    /// True if any user record exists. Used by the bootstrap path to
    /// decide whether to mint the root operator at first run.
    async fn has_any_user(&self) -> Result<bool, StoreError>;

    // ------------------------------------------------------------------
    // API keys
    // ------------------------------------------------------------------

    /// Persist a freshly issued API key (storage form: bcrypt hash).
    async fn create_api_key(&self, key: ApiKey) -> Result<ApiKey, StoreError>;

    /// List the API keys belonging to a single user. Used by `tcadm
    /// api-key list`.
    async fn list_api_keys(&self, user_id: Uuid) -> Result<Vec<ApiKey>, StoreError>;

    /// Look up an API key by its non-secret `lookup_id` segment.
    /// Returns [`StoreError::NotFound`] if no such key exists. The
    /// auth middleware uses this for O(1) credential resolution.
    async fn get_api_key_by_lookup_id(&self, lookup_id: &str) -> Result<ApiKey, StoreError>;

    /// Delete an API key by id. Returns [`StoreError::NotFound`] if
    /// the id does not belong to `user_id`'s set.
    async fn delete_api_key(&self, user_id: Uuid, key_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // System keys
    // ------------------------------------------------------------------

    /// Read a cluster-level system key, e.g. the JWT signing secret.
    async fn get_system_key(&self, key: SystemKey) -> Result<Vec<u8>, StoreError>;

    /// Persist a cluster-level system key. Overwrites any existing
    /// value; rotation policy lives in the caller.
    async fn put_system_key(&self, key: SystemKey, value: Vec<u8>) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Federated users (OIDC)
    // ------------------------------------------------------------------

    /// Look up a user by their `(silo_id, issuer, subject)` triple.
    /// Returns [`StoreError::NotFound`] if no user matches; the auth
    /// middleware uses that to JIT-create the row on first OIDC
    /// login.
    async fn get_user_by_federation(
        &self,
        silo_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<User, StoreError>;

    // ------------------------------------------------------------------
    // Per-silo IdP configuration
    // ------------------------------------------------------------------

    /// Persist (or replace) the OIDC IdP config for a silo. Eager
    /// discovery happens in the caller before this is invoked, so
    /// failure here is purely storage-side.
    async fn put_idp_config(
        &self,
        silo_id: Uuid,
        config: IdpConfig,
    ) -> Result<IdpConfig, StoreError>;

    /// Read the IdP config for a silo. Returns [`StoreError::NotFound`]
    /// when the silo has no IdP attached.
    async fn get_idp_config(&self, silo_id: Uuid) -> Result<IdpConfig, StoreError>;

    /// Remove the IdP config for a silo.
    async fn delete_idp_config(&self, silo_id: Uuid) -> Result<(), StoreError>;

    /// Iterate every (silo_id, IdpConfig) pair. Used by the auth
    /// middleware to find the IdP whose `issuer` matches an inbound
    /// token's `iss` claim.
    async fn list_idp_configs(&self) -> Result<Vec<(Uuid, IdpConfig)>, StoreError>;

    // ------------------------------------------------------------------
    // Projects (silo-scoped)
    // ------------------------------------------------------------------

    /// Create a project inside a silo. Returns
    /// [`StoreError::Conflict`] if `name` is already in use within
    /// the same silo. Returns [`StoreError::NotFound`] if the silo
    /// itself doesn't exist (the caller is expected to have already
    /// resolved silo existence via Cedar; the check here is a
    /// defence-in-depth race guard).
    async fn create_project(&self, silo_id: Uuid, req: NewProject) -> Result<Project, StoreError>;

    /// Look up a project by id. Returns [`StoreError::NotFound`] when
    /// no such project exists, regardless of silo.
    async fn get_project(&self, project_id: Uuid) -> Result<Project, StoreError>;

    /// List every project belonging to `silo_id`. Order is unspecified
    /// for Phase 0e-c; pagination lands when the list grows beyond a
    /// single response.
    async fn list_projects_in_silo(&self, silo_id: Uuid) -> Result<Vec<Project>, StoreError>;

    /// Delete a project by id. Returns [`StoreError::NotFound`] if the
    /// id does not exist.
    async fn delete_project(&self, project_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // VPCs (project-scoped)
    // ------------------------------------------------------------------

    /// Create a VPC inside a project.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * The project must exist and `project.silo_id == silo_id`. A
    ///   `silo_id` mismatch returns [`StoreError::NotFound`] (treating
    ///   the project as invisible to the wrong silo).
    /// * `name` must not collide with an existing VPC in the same
    ///   project — collision returns [`StoreError::Conflict`].
    /// * `vni` is server-assigned, drawn uniformly at random from
    ///   `[VPC_VNI_RESERVED_CEILING, VPC_VNI_MAX)`, with
    ///   collision-retry against the rack-wide VNI index. If the
    ///   retry loop is exhausted (operationally unreachable),
    ///   returns [`StoreError::Backend`].
    /// * The caller is expected to have validated `req.ipv4_block.is_some()
    ///   || req.ipv6_block.is_some()` at the API layer; the store
    ///   does not re-validate.
    async fn create_vpc(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewVpc,
    ) -> Result<Vpc, StoreError>;

    /// Look up a VPC by id. Returns [`StoreError::NotFound`] when no
    /// such VPC exists, regardless of silo or project. Handlers add
    /// silo_id + project_id rechecks on top.
    async fn get_vpc(&self, vpc_id: Uuid) -> Result<Vpc, StoreError>;

    /// List every VPC belonging to `project_id`. Order is unspecified.
    /// Returns an empty vec if the project has no VPCs *or* if the
    /// project does not exist — distinguishing the two would require
    /// a project-existence read the caller has already done.
    async fn list_vpcs_in_project(&self, project_id: Uuid) -> Result<Vec<Vpc>, StoreError>;

    /// Delete a VPC by id. Returns [`StoreError::NotFound`] if the
    /// id does not exist. Returns [`StoreError::Conflict`] if the
    /// VPC still has subnets attached — the operator must clear
    /// subnets before deleting the VPC. (No cascade in Phase 0;
    /// preserves the "don't accidentally lose tenant data" stance.)
    async fn delete_vpc(&self, vpc_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Subnets (vpc-scoped)
    // ------------------------------------------------------------------

    /// Create a subnet inside a VPC.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * The VPC must exist *and* `vpc.silo_id == silo_id` *and*
    ///   `vpc.project_id == project_id`. Any mismatch returns
    ///   [`StoreError::NotFound`] — the caller cannot tell whether
    ///   the VPC is in a different parent or doesn't exist at all,
    ///   which is the cross-tenant probe story we want.
    /// * At least one of `req.ipv4_block` / `req.ipv6_block` must
    ///   be `Some`. The API layer enforces this before calling the
    ///   store; the store does not re-validate.
    /// * Each present family CIDR must be a subnet of the parent
    ///   VPC's same-family CIDR. Each present family must also be
    ///   present on the VPC. Violations return
    ///   [`StoreError::Conflict`] (with a message naming the
    ///   broken invariant).
    /// * No present family CIDR may overlap an existing subnet's
    ///   CIDR in the same VPC. Overlap → [`StoreError::Conflict`].
    /// * `name` must not collide with an existing subnet in the
    ///   same VPC.
    async fn create_subnet(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewSubnet,
    ) -> Result<Subnet, StoreError>;

    /// Look up a subnet by id. Returns [`StoreError::NotFound`] when
    /// no such subnet exists. Handlers add silo_id + project_id +
    /// vpc_id rechecks on top.
    async fn get_subnet(&self, subnet_id: Uuid) -> Result<Subnet, StoreError>;

    /// List every subnet in a VPC. Order is unspecified.
    async fn list_subnets_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<Subnet>, StoreError>;

    /// Delete a subnet by id. Returns [`StoreError::NotFound`] if the
    /// id does not exist.
    async fn delete_subnet(&self, subnet_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // SSH keys (silo-scoped catalog)
    // ------------------------------------------------------------------

    /// Register an SSH key in a silo's catalog.
    ///
    /// The caller (the API layer) is responsible for parsing
    /// `req.public_key` as openssh and computing the canonical
    /// SHA-256 fingerprint. tritond-store stays free of ssh-key
    /// crate dependencies; the store treats `public_key` as opaque
    /// and trusts the supplied `fingerprint`.
    ///
    /// The store enforces:
    ///
    /// * The silo exists. Missing silo → [`StoreError::NotFound`].
    /// * `name` is unique within the silo. Collision →
    ///   [`StoreError::Conflict`].
    /// * `fingerprint` is unique within the silo (re-uploading the
    ///   same key under a new name is a Conflict so the catalog
    ///   doesn't accumulate aliased pool entries).
    async fn create_ssh_key(
        &self,
        silo_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError>;

    /// Look up an SSH key by id. Returns [`StoreError::NotFound`]
    /// when no such key exists, regardless of silo.
    async fn get_ssh_key(&self, key_id: Uuid) -> Result<SshKey, StoreError>;

    /// List every SSH key registered in a silo's catalog.
    async fn list_ssh_keys_in_silo(&self, silo_id: Uuid) -> Result<Vec<SshKey>, StoreError>;

    /// Delete an SSH key by id. Returns [`StoreError::NotFound`] if
    /// the id does not exist.
    async fn delete_ssh_key(&self, key_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Images (silo-scoped catalog)
    // ------------------------------------------------------------------

    /// Register an image in a silo's catalog.
    ///
    /// The store enforces:
    ///
    /// * The silo exists. Missing silo → [`StoreError::NotFound`].
    /// * `name` is unique within the silo. Collision →
    ///   [`StoreError::Conflict`].
    /// * `(name, version)` is treated as a single addressable
    ///   tuple by some operator workflows; uniqueness is on `name`
    ///   alone for Phase 0 — operators encode versions into the
    ///   name (e.g. `ubuntu-22.04-base`) until a registry-style
    ///   model lands.
    ///
    /// The caller is expected to have validated `req.sha256`
    /// (lowercase 64-char hex) at the API edge. The store treats
    /// it as an opaque string.
    async fn create_image(&self, silo_id: Uuid, req: NewImage) -> Result<Image, StoreError>;

    /// Look up an image by id. Returns [`StoreError::NotFound`]
    /// when no such image exists, regardless of silo.
    async fn get_image(&self, image_id: Uuid) -> Result<Image, StoreError>;

    /// List every image in a silo's catalog.
    async fn list_images_in_silo(&self, silo_id: Uuid) -> Result<Vec<Image>, StoreError>;

    /// Delete an image by id.
    async fn delete_image(&self, image_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Project quotas (singleton per project)
    // ------------------------------------------------------------------

    /// Set (or replace) a project's quota record. Returns
    /// [`StoreError::NotFound`] if the project does not exist or
    /// does not live in the supplied silo (cross-tenant probe
    /// invariant).
    async fn put_quota(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewQuota,
    ) -> Result<Quota, StoreError>;

    /// Read a project's quota. Returns [`StoreError::NotFound`] if
    /// the project does not exist, lives in a different silo, or
    /// has no quota set.
    async fn get_quota(&self, silo_id: Uuid, project_id: Uuid) -> Result<Quota, StoreError>;

    /// Remove a project's quota (project becomes unlimited). Returns
    /// [`StoreError::NotFound`] if no quota was set.
    async fn delete_quota(&self, silo_id: Uuid, project_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Instances (project-scoped, with lifecycle state machine)
    // ------------------------------------------------------------------

    /// Create an instance, atomically creating its primary NIC and
    /// boot Disk.
    ///
    /// The store enforces structural invariants:
    ///
    /// * Project exists and `project.silo_id == silo_id`. Mismatch
    ///   surfaces as [`StoreError::NotFound`] (cross-tenant probe
    ///   story).
    /// * Image exists and `image.silo_id == silo_id`. A missing or
    ///   wrong-silo image is [`StoreError::NotFound`].
    /// * Subnet exists and lives in this project (i.e.
    ///   `subnet.silo_id == silo_id` and `subnet.project_id ==
    ///   project_id`). Otherwise [`StoreError::NotFound`].
    /// * Each `ssh_key_id` exists and `silo_id` matches. Otherwise
    ///   [`StoreError::NotFound`].
    /// * `name` is unique within the project. Collision →
    ///   [`StoreError::Conflict`].
    ///
    /// On success the instance is created with `lifecycle =
    /// Pending`, the primary NIC named `"primary"` is allocated
    /// (MAC randomly generated; IPv4/IPv6 from the parent subnet),
    /// and the boot Disk named `"boot"` is created sized to the
    /// source image and tagged with that image's id. All three
    /// records are written in a single transaction — either all
    /// exist or none do. Subnet IP exhaustion → [`StoreError::Backend`]
    /// (operationally unreachable for v0).
    ///
    /// The caller is expected to have validated `cpu > 0` and
    /// `memory_bytes > 0` at the API edge.
    async fn create_instance(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewInstance,
    ) -> Result<InstanceCreateResult, StoreError>;

    /// Look up an instance by id.
    async fn get_instance(&self, instance_id: Uuid) -> Result<Instance, StoreError>;

    /// List every instance in a project.
    async fn list_instances_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Instance>, StoreError>;

    /// Delete an instance. Returns [`StoreError::Conflict`] if the
    /// current lifecycle is not deletable
    /// (see [`LifecycleState::is_deletable`]) — operators must
    /// stop a Running instance before deletion. Returns
    /// [`StoreError::NotFound`] if the id does not exist.
    ///
    /// Cascades to the instance's NICs and Disks: each NIC record
    /// is removed, its IPv4 / IPv6 allocations freed back to the
    /// parent subnet's pool, every disk record is removed, all in
    /// the same transaction.
    /// Delete an instance and cascade its NICs, Disks, and any
    /// FloatingIp attachments. The store enforces the
    /// "deletable lifecycle" rule (Stopped or Failed only) by
    /// default; pass `force = true` to skip the gate, used by
    /// the `?force=true` operator override on the
    /// `instance_delete` HTTP handler.
    async fn delete_instance(&self, instance_id: Uuid, force: bool) -> Result<(), StoreError>;

    /// Atomic compare-and-set on an instance's lifecycle. Reads the
    /// current state; if its discriminant is in `expected_from`,
    /// transitions to `to` and bumps `updated_at`. Otherwise
    /// returns [`StoreError::Conflict`] naming the observed state.
    ///
    /// `expected_from` takes [`LifecycleStateKind`] (discriminants
    /// only) so callers can name "any Failed state" without
    /// committing to a specific `reason`.
    ///
    /// This is the building block all start/stop/restart and the
    /// (future) agent-driven Pending → Provisioning → Running
    /// transitions are written on top of.
    async fn transition_instance_lifecycle(
        &self,
        instance_id: Uuid,
        expected_from: &[LifecycleStateKind],
        to: LifecycleState,
    ) -> Result<Instance, StoreError>;

    // ------------------------------------------------------------------
    // NICs (instance-scoped, auto-created at instance create)
    // ------------------------------------------------------------------

    /// Look up a NIC by id. Returns [`StoreError::NotFound`] when
    /// no such NIC exists. Handlers add silo + project + instance
    /// id rechecks on top.
    async fn get_nic(&self, nic_id: Uuid) -> Result<Nic, StoreError>;

    /// List the NICs attached to a single instance. Order is
    /// unspecified; Phase 0 produces exactly one NIC per instance
    /// (the auto-created `"primary"`).
    async fn list_nics_for_instance(&self, instance_id: Uuid) -> Result<Vec<Nic>, StoreError>;

    // ------------------------------------------------------------------
    // Disks (instance-scoped, auto-created at instance create)
    // ------------------------------------------------------------------

    /// Look up a Disk by id. Returns [`StoreError::NotFound`] when
    /// no such Disk exists. Handlers add silo + project + instance
    /// id rechecks on top.
    async fn get_disk(&self, disk_id: Uuid) -> Result<Disk, StoreError>;

    /// List the Disks attached to a single instance. Order is
    /// unspecified; Phase 0 produces exactly one Disk per instance
    /// (the auto-created `"boot"`).
    async fn list_disks_for_instance(&self, instance_id: Uuid) -> Result<Vec<Disk>, StoreError>;

    // ------------------------------------------------------------------
    // Floating IPs (project-scoped, allocated from a fleet pool)
    // ------------------------------------------------------------------

    /// Allocate a [`FloatingIp`] from the requested family's
    /// hardcoded Phase 0 pool.
    ///
    /// Invariants:
    ///
    /// * The project exists and `project.silo_id == silo_id`.
    ///   Otherwise [`StoreError::NotFound`].
    /// * `name` is unique within the project. Collision →
    ///   [`StoreError::Conflict`].
    /// * Pool exhaustion (operationally unreachable for v0 with
    ///   /24 + /48 pools) → [`StoreError::Backend`].
    ///
    /// The returned `FloatingIp` starts unattached
    /// (`attached_to == None`).
    async fn create_floating_ip(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewFloatingIp,
    ) -> Result<FloatingIp, StoreError>;

    /// Look up a FloatingIp by id. Handlers add silo + project
    /// rechecks on top.
    async fn get_floating_ip(&self, fip_id: Uuid) -> Result<FloatingIp, StoreError>;

    /// List every FloatingIp owned by a project.
    async fn list_floating_ips_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<FloatingIp>, StoreError>;

    /// Release a FloatingIp back to its pool. Returns
    /// [`StoreError::Conflict`] if the IP is currently attached
    /// (operator must detach first); a future force-delete path
    /// could detach + release in one call.
    async fn delete_floating_ip(&self, fip_id: Uuid) -> Result<(), StoreError>;

    /// Atomically attach a FloatingIp to a NIC, replacing any
    /// existing attachment. The target NIC must live in the same
    /// silo + project as the FloatingIp; mismatch surfaces as
    /// [`StoreError::NotFound`].
    ///
    /// "Replace" semantics: if the FloatingIp was already attached
    /// to a different NIC, the new attachment swaps in place
    /// inside one transaction — operators see a single before/
    /// after state with no detached window.
    async fn attach_floating_ip(
        &self,
        fip_id: Uuid,
        target_nic_id: Uuid,
    ) -> Result<FloatingIp, StoreError>;

    /// Clear the FloatingIp's `attached_to`. No-op (returns the
    /// current record) if already detached. The IP stays owned by
    /// the project.
    async fn detach_floating_ip(&self, fip_id: Uuid) -> Result<FloatingIp, StoreError>;

    // ------------------------------------------------------------------
    // Provisioning jobs (FIFO queue consumed by an agent)
    // ------------------------------------------------------------------

    /// Append a job to the queue. Server assigns `id`, `seq`
    /// (monotonic, FIFO order), and `created_at`. Initial status
    /// is [`JobStatus::Pending`].
    async fn enqueue_job(&self, req: NewJob) -> Result<ProvisioningJob, StoreError>;

    /// Return every job currently in [`JobStatus::InProgress`]
    /// whose `claimed_at` is older than `now - cutoff`. Used by
    /// the tritond stale-claim sweeper to identify jobs an
    /// agent claimed but never completed (agent crashed, host
    /// rebooted, network partition); the sweeper then transitions
    /// those jobs to terminal `Failed` so the operator-visible
    /// state catches up.
    ///
    /// Implementations may scan the entire job table; Phase 0
    /// queue sizes are small enough that we accept the cost.
    async fn list_stale_claims(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ProvisioningJob>, StoreError>;

    /// Atomically claim the next Pending job (lowest `seq`),
    /// transitioning it to [`JobStatus::InProgress`] and stamping
    /// `claimed_at` + `claimed_by`. Returns
    /// [`StoreError::NotFound`] if the queue has no Pending jobs.
    ///
    /// `claimed_by` is a free-form identifier the agent picks
    /// (e.g. `"stub-provisioner"` for the in-process stub).
    async fn claim_next_job(&self, claimed_by: &str) -> Result<ProvisioningJob, StoreError>;

    /// Mark a job as terminal (Completed or Failed). Stamps
    /// `completed_at`. Returns [`StoreError::NotFound`] if the job
    /// does not exist; [`StoreError::Conflict`] if it is already
    /// terminal (Completed or Failed).
    async fn complete_job(
        &self,
        job_id: Uuid,
        outcome: JobOutcome,
    ) -> Result<ProvisioningJob, StoreError>;

    /// Look up a single job by id.
    async fn get_job(&self, job_id: Uuid) -> Result<ProvisioningJob, StoreError>;

    /// List the most recent `limit` jobs across all statuses, in
    /// reverse chronological order (newest first). Used by
    /// operator debugging surfaces.
    async fn list_recent_jobs(&self, limit: usize) -> Result<Vec<ProvisioningJob>, StoreError>;
}
