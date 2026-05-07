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
    AUTO_APPROVE_WINDOW_MAX, AddressFamily, ApiKey, ApiKeyScope, ApiKeyView, AutoApproveWindow,
    BHYVE_M1_MIN_BOOT_DISK_BYTES, CLAIM_CODE_ALPHABET, CLAIM_CODE_LEN, CLAIM_CODE_TTL, Cn, CnRole,
    CnState, CnView, Disk, DiskKind, EdgeCluster, EdgeClusterInstance, EdgeClusterInstanceState,
    EdgeClusterKind, EdgeClusterResource, EdgeNicCoord, FLOATING_IP_V4_POOL, FLOATING_IP_V6_POOL,
    Federation, FloatingIp, FloatingIpAttachment, IdpConfig, IdpConfigView, Image,
    ImageCompatibility, ImageScope, Instance, InstanceCreateResult, IpCidr, JobKind, JobOutcome,
    JobStatus, JobStatusKind, LifecycleState, LifecycleStateKind, NatGateway, NetworkResourceId,
    NewEdgeCluster, NewFloatingIp, NewImage, NewInstance, NewInstanceNic, NewJob, NewNatGateway,
    NewProject, NewQuota, NewRoute, NewRouteTable, NewSilo, NewSshKey, NewSubnet, NewTenant,
    NewVpc, Nic, Project, ProvisioningJob, Quota, Realization, RealizationStatus,
    RealizedNetworkState, RealizerId, Route, RouteTable, RouteTarget, Silo, SshKey, SshKeyScope,
    Subnet, SystemKey, TRITOND_IMAGE_NAMESPACE, TRITOND_SSH_KEY_NAMESPACE, Tenant, User, UserView,
    VPC_VNI_MAX, VPC_VNI_RESERVED_CEILING, Vpc, default_boot_disk_size_bytes, derive_image_id,
    derive_ssh_key_id, format_claim_code, generate_claim_code, generate_poll_token,
    normalize_claim_code,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
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

    /// Replace an operator account's password hash.
    ///
    /// Returns the updated user. This is intentionally hash-only so
    /// callers remain responsible for password generation, hashing
    /// policy, and one-time secret display.
    async fn update_user_password_hash(
        &self,
        username: &str,
        password_hash: String,
    ) -> Result<User, StoreError>;

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

    /// Look up a user by their `(tenant_id, issuer, subject)` triple.
    /// As of E-5 the IdP is tenant-scoped, so the federation index is
    /// keyed by the tenant that owns the IdP — the same tenant the
    /// JIT-created [`User`] is rooted under via [`User::tenant_id`].
    ///
    /// Returns [`StoreError::NotFound`] if no user matches; the
    /// auth middleware uses that to JIT-create the row on first
    /// OIDC login.
    async fn get_user_by_federation(
        &self,
        tenant_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<User, StoreError>;

    // ------------------------------------------------------------------
    // Per-tenant IdP configuration
    // ------------------------------------------------------------------

    /// Persist (or replace) the OIDC IdP config for a tenant. Eager
    /// discovery happens in the caller before this is invoked, so
    /// failure here is purely storage-side.
    ///
    /// Enforces issuer uniqueness across all tenants: if the
    /// supplied `config.issuer_url` is already claimed by a
    /// *different* tenant, returns [`StoreError::Conflict`].
    /// Re-putting the same tenant's config (idempotent or with a
    /// changed issuer) is fine.
    async fn put_idp_config(
        &self,
        tenant_id: Uuid,
        config: IdpConfig,
    ) -> Result<IdpConfig, StoreError>;

    /// Read the IdP config for a tenant. Returns [`StoreError::NotFound`]
    /// when the tenant has no IdP attached.
    async fn get_idp_config(&self, tenant_id: Uuid) -> Result<IdpConfig, StoreError>;

    /// Remove the IdP config for a tenant.
    async fn delete_idp_config(&self, tenant_id: Uuid) -> Result<(), StoreError>;

    /// Iterate every (tenant_id, IdpConfig) pair. Returned `Uuid` is
    /// now the owning tenant id (post E-5). The
    /// [`Self::get_idp_config_by_issuer`] index is the preferred
    /// lookup path; this method exists for operator surfaces that
    /// dump every configured IdP.
    async fn list_idp_configs(&self) -> Result<Vec<(Uuid, IdpConfig)>, StoreError>;

    /// Look up the (tenant_id, IdpConfig) pair whose issuer
    /// matches `issuer`, if any. Used by `authenticate_oidc` to
    /// route an inbound token to its owning tenant. Returns
    /// [`StoreError::NotFound`] when no IdP claims the issuer.
    async fn get_idp_config_by_issuer(&self, issuer: &str)
    -> Result<(Uuid, IdpConfig), StoreError>;

    // ------------------------------------------------------------------
    // Projects (tenant-scoped)
    // ------------------------------------------------------------------

    /// Create a project inside a tenant. Returns
    /// [`StoreError::Conflict`] if `name` is already in use within
    /// the same tenant. Returns [`StoreError::NotFound`] if the
    /// tenant itself doesn't exist (the caller is expected to have
    /// already resolved tenant existence via Cedar; the check here
    /// is a defence-in-depth race guard).
    async fn create_project(&self, tenant_id: Uuid, req: NewProject)
    -> Result<Project, StoreError>;

    /// Look up a project by id. Returns [`StoreError::NotFound`] when
    /// no such project exists, regardless of tenant.
    async fn get_project(&self, project_id: Uuid) -> Result<Project, StoreError>;

    /// List every project belonging to `tenant_id`. Order is
    /// unspecified; pagination lands when the list grows beyond a
    /// single response.
    async fn list_projects_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Project>, StoreError>;

    /// Delete a project by id. Returns [`StoreError::NotFound`] if the
    /// id does not exist.
    async fn delete_project(&self, project_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Tenants (silo-scoped customer containers)
    // ------------------------------------------------------------------

    /// Create a new tenant inside a silo. Returns
    /// [`StoreError::Conflict`] if `req.name` is already in use
    /// within the silo. Returns [`StoreError::NotFound`] if the
    /// silo doesn't exist.
    async fn create_tenant(&self, silo_id: Uuid, req: NewTenant) -> Result<Tenant, StoreError>;

    /// Look up a tenant by id. Returns [`StoreError::NotFound`]
    /// when no such tenant exists.
    async fn get_tenant(&self, tenant_id: Uuid) -> Result<Tenant, StoreError>;

    /// List every tenant owned by a silo. Returns an empty Vec
    /// (not NotFound) when the silo exists but has no tenants;
    /// returns NotFound when the silo itself doesn't exist.
    async fn list_tenants_in_silo(&self, silo_id: Uuid) -> Result<Vec<Tenant>, StoreError>;

    /// Delete a tenant by id. Returns [`StoreError::NotFound`]
    /// if the tenant doesn't exist; [`StoreError::Conflict`] if
    /// the tenant still has child projects (no cascading deletes
    /// in Phase 0 — locked decision #17).
    async fn delete_tenant(&self, tenant_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // VPCs (project-scoped)
    // ------------------------------------------------------------------

    /// Create a VPC inside a project.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * The project must exist and `project.tenant_id == tenant_id`.
    ///   A `tenant_id` mismatch returns [`StoreError::NotFound`]
    ///   (treating the project as invisible to the wrong tenant).
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
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewVpc,
    ) -> Result<Vpc, StoreError>;

    /// Look up a VPC by id. Returns [`StoreError::NotFound`] when no
    /// such VPC exists, regardless of tenant or project. Handlers add
    /// tenant_id + project_id rechecks on top.
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
    /// * The VPC must exist *and* `vpc.tenant_id == tenant_id` *and*
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
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewSubnet,
    ) -> Result<Subnet, StoreError>;

    /// Look up a subnet by id. Returns [`StoreError::NotFound`] when
    /// no such subnet exists. Handlers add tenant_id + project_id +
    /// vpc_id rechecks on top.
    async fn get_subnet(&self, subnet_id: Uuid) -> Result<Subnet, StoreError>;

    /// List every subnet in a VPC. Order is unspecified.
    async fn list_subnets_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<Subnet>, StoreError>;

    /// Delete a subnet by id. Returns [`StoreError::NotFound`] if the
    /// id does not exist.
    async fn delete_subnet(&self, subnet_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Route tables (vpc-scoped)
    // ------------------------------------------------------------------

    /// Create an additional route table inside a VPC.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * The VPC must exist and match `tenant_id` + `project_id`.
    ///   Mismatch returns [`StoreError::NotFound`] to preserve the
    ///   cross-tenant probe invariant.
    /// * `name` must be unique within the VPC. The auto-created
    ///   main route table reserves the name `main`.
    async fn create_route_table(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewRouteTable,
    ) -> Result<RouteTable, StoreError>;

    /// Look up a route table by id. Handlers add tenant + project +
    /// VPC rechecks on top.
    async fn get_route_table(&self, route_table_id: Uuid) -> Result<RouteTable, StoreError>;

    /// List every route table in a VPC.
    async fn list_route_tables_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<RouteTable>, StoreError>;

    /// Delete a route table. Main route tables and tables still
    /// referenced by subnets return [`StoreError::Conflict`].
    async fn delete_route_table(&self, route_table_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Routes (route-table-scoped)
    // ------------------------------------------------------------------

    /// Create a route inside a route table.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * The route table must exist and match `tenant_id`,
    ///   `project_id`, and `vpc_id`. Any mismatch returns
    ///   [`StoreError::NotFound`] to preserve the cross-tenant probe
    ///   invariant.
    /// * `destination` is canonicalized before uniqueness checks.
    /// * Destination family must be present on the parent VPC.
    /// * Destination CIDR must be unique within the route table.
    /// * `RouteTarget::NatGateway` must reference a NAT gateway in
    ///   the same VPC.
    async fn create_route(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        route_table_id: Uuid,
        req: NewRoute,
    ) -> Result<Route, StoreError>;

    /// Look up a route by id. Handlers add tenant + project + VPC +
    /// route-table rechecks on top.
    async fn get_route(&self, route_id: Uuid) -> Result<Route, StoreError>;

    /// List every route in a route table.
    async fn list_routes_in_table(&self, route_table_id: Uuid) -> Result<Vec<Route>, StoreError>;

    /// Delete a route by id.
    async fn delete_route(&self, route_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // NAT gateways (vpc-scoped, allocated from the shared public pool)
    // ------------------------------------------------------------------

    /// Create a NAT gateway inside a VPC.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * The VPC must exist and match `tenant_id` + `project_id`.
    ///   Mismatch returns [`StoreError::NotFound`] to preserve the
    ///   cross-tenant probe invariant.
    /// * `name` must be unique within the VPC.
    /// * `public_address` is allocated from the same Phase 0 pool
    ///   used by [`FloatingIp`], so FIP and NAT addresses cannot
    ///   collide.
    /// * `desired_generation` starts at 1. Future wire-affecting
    ///   mutations increment it atomically.
    async fn create_nat_gateway(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewNatGateway,
    ) -> Result<NatGateway, StoreError>;

    /// Look up a NAT gateway by id. Handlers add tenant + project +
    /// VPC rechecks on top.
    async fn get_nat_gateway(&self, nat_gateway_id: Uuid) -> Result<NatGateway, StoreError>;

    /// List every NAT gateway in a VPC.
    async fn list_nat_gateways_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<NatGateway>, StoreError>;

    /// Delete a NAT gateway and release its public address. The
    /// referencing-route guard lands with the Route slice because
    /// routes do not exist yet in H-2.
    async fn delete_nat_gateway(&self, nat_gateway_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Edge clusters
    // ------------------------------------------------------------------

    /// Create a durable edge cluster. v1 placement creates one
    /// `EdgeClusterKind::NatGateway` cluster per NAT gateway; the
    /// schema supports additional instances and resource kinds without
    /// changing the parent shape.
    ///
    /// Invariants enforced by the implementation:
    ///
    /// * `name` is globally unique across edge clusters.
    /// * every bound resource exists.
    /// * the cluster kind must accept every bound resource.
    /// * duplicate bound resources in one create request conflict.
    /// * `desired_generation` starts at 1.
    async fn create_edge_cluster(&self, req: NewEdgeCluster) -> Result<EdgeCluster, StoreError>;

    /// Look up an edge cluster by id.
    async fn get_edge_cluster(&self, edge_cluster_id: Uuid) -> Result<EdgeCluster, StoreError>;

    /// List every edge cluster.
    async fn list_edge_clusters(&self) -> Result<Vec<EdgeCluster>, StoreError>;

    /// List edge clusters bound to `resource`.
    async fn list_edge_clusters_for_resource(
        &self,
        resource: EdgeClusterResource,
    ) -> Result<Vec<EdgeCluster>, StoreError>;

    /// Delete an edge cluster and remove its name and resource
    /// indexes.
    async fn delete_edge_cluster(&self, edge_cluster_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // SSH keys (multi-scope catalog: Public / Silo / Tenant / Project / User)
    // ------------------------------------------------------------------

    /// Register a `Public` SSH key (visible to everyone). The
    /// caller (the API layer) is responsible for parsing
    /// `req.public_key` as openssh and computing the canonical
    /// SHA-256 fingerprint. tritond-store stays free of ssh-key
    /// crate dependencies; the store treats `public_key` as opaque
    /// and trusts the supplied `fingerprint`.
    ///
    /// The store enforces:
    /// * `name` is unique among Public keys. Collision →
    ///   [`StoreError::Conflict`].
    /// * `fingerprint` is unique among Public keys (re-uploading
    ///   the same key under a new name is a Conflict so the catalog
    ///   doesn't accumulate aliased pool entries).
    async fn create_ssh_key_public(
        &self,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError>;

    /// Register a `Silo`-scoped SSH key. Returns
    /// [`StoreError::NotFound`] if the silo does not exist;
    /// [`StoreError::Conflict`] if `name` or `fingerprint` is
    /// already in use among the silo's silo-scoped keys.
    async fn create_ssh_key_silo(
        &self,
        silo_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError>;

    /// Register a `Tenant`-scoped SSH key. Returns
    /// [`StoreError::NotFound`] if the tenant does not exist;
    /// [`StoreError::Conflict`] if `name` or `fingerprint` is
    /// already in use among the tenant's tenant-scoped keys.
    async fn create_ssh_key_tenant(
        &self,
        tenant_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError>;

    /// Register a `Project`-scoped SSH key. Returns
    /// [`StoreError::NotFound`] if the project does not exist;
    /// [`StoreError::Conflict`] if `name` or `fingerprint` is
    /// already in use among the project's project-scoped keys.
    async fn create_ssh_key_project(
        &self,
        project_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError>;

    /// Register a `User`-scoped SSH key. Returns
    /// [`StoreError::NotFound`] if the user does not exist;
    /// [`StoreError::Conflict`] if `name` or `fingerprint` is
    /// already in use among the user's user-scoped keys.
    async fn create_ssh_key_user(
        &self,
        user_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError>;

    /// Look up an SSH key by id. Returns [`StoreError::NotFound`]
    /// when no such key exists, regardless of scope. The handler
    /// is expected to apply the visibility predicate on top —
    /// this method is the cross-scope identity lookup.
    async fn get_ssh_key(&self, key_id: Uuid) -> Result<SshKey, StoreError>;

    /// List every Public SSH key. Equivalent to filtering
    /// `get_ssh_key` over `SshKeyScope::Public`.
    async fn list_ssh_keys_public(&self) -> Result<Vec<SshKey>, StoreError>;

    /// List every SSH key whose scope is exactly `Silo { silo_id }`.
    /// Does NOT include Public; the caller composes unions via
    /// [`Self::list_visible_ssh_keys_in_tenant`] /
    /// [`Self::list_visible_ssh_keys_in_project`].
    async fn list_ssh_keys_in_silo(&self, silo_id: Uuid) -> Result<Vec<SshKey>, StoreError>;

    /// List every SSH key whose scope is exactly `Tenant { tenant_id }`.
    async fn list_ssh_keys_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<SshKey>, StoreError>;

    /// List every SSH key whose scope is exactly `Project { project_id }`.
    async fn list_ssh_keys_in_project(&self, project_id: Uuid) -> Result<Vec<SshKey>, StoreError>;

    /// List every SSH key whose scope is exactly `User { user_id }`.
    async fn list_ssh_keys_for_user(&self, user_id: Uuid) -> Result<Vec<SshKey>, StoreError>;

    /// List every SSH key visible at a tenant URL: Public + Silo
    /// (of tenant's silo) + Tenant.
    async fn list_visible_ssh_keys_in_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<SshKey>, StoreError>;

    /// List every SSH key visible at a project URL: Public + Silo
    /// (of project's silo) + Tenant (of project's tenant) +
    /// Project.
    async fn list_visible_ssh_keys_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<SshKey>, StoreError>;

    /// Delete an SSH key by id. Visibility / ownership gating is
    /// applied at the handler layer; the store layer just removes
    /// the record. Returns [`StoreError::NotFound`] if the id
    /// does not exist.
    async fn delete_ssh_key(&self, key_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Images (multi-scope catalog: Public / Silo / Tenant / Project / User)
    // ------------------------------------------------------------------

    /// Register a `Public` image (visible to everyone, including
    /// anonymous probes on the public listing endpoint).
    ///
    /// The store enforces:
    /// * `name` is unique among Public images. Collision →
    ///   [`StoreError::Conflict`].
    ///
    /// The caller is expected to have validated `req.sha256`
    /// (lowercase 64-char hex) at the API edge. The store treats
    /// it as an opaque string.
    async fn create_image_public(&self, req: NewImage) -> Result<Image, StoreError>;

    /// Register a `Silo`-scoped image. Returns
    /// [`StoreError::NotFound`] if the silo does not exist;
    /// [`StoreError::Conflict`] if `name` is already in use among
    /// the silo's silo-scoped images.
    async fn create_image_silo(&self, silo_id: Uuid, req: NewImage) -> Result<Image, StoreError>;

    /// Register a `Tenant`-scoped image. Returns
    /// [`StoreError::NotFound`] if the tenant does not exist;
    /// [`StoreError::Conflict`] if `name` is already in use
    /// among the tenant's tenant-scoped images.
    async fn create_image_tenant(
        &self,
        tenant_id: Uuid,
        req: NewImage,
    ) -> Result<Image, StoreError>;

    /// Register a `Project`-scoped image. Returns
    /// [`StoreError::NotFound`] if the project does not exist;
    /// [`StoreError::Conflict`] if `name` is already in use
    /// among the project's project-scoped images.
    async fn create_image_project(
        &self,
        project_id: Uuid,
        req: NewImage,
    ) -> Result<Image, StoreError>;

    /// Register a `User`-scoped image. Returns
    /// [`StoreError::NotFound`] if the user does not exist;
    /// [`StoreError::Conflict`] if `name` is already in use
    /// among the user's user-scoped images.
    async fn create_image_user(&self, user_id: Uuid, req: NewImage) -> Result<Image, StoreError>;

    /// Look up an image by id. Returns [`StoreError::NotFound`]
    /// when no such image exists, regardless of scope. The
    /// handler is expected to apply the visibility predicate on
    /// top — this method is the cross-scope identity lookup.
    async fn get_image(&self, image_id: Uuid) -> Result<Image, StoreError>;

    /// List every Public image. Equivalent to filtering
    /// `get_image` over `ImageScope::Public`.
    async fn list_images_public(&self) -> Result<Vec<Image>, StoreError>;

    /// List every image whose scope is exactly `Silo { silo_id }`.
    /// Does NOT include Public; the caller composes unions via
    /// [`Self::list_visible_images_in_tenant`] /
    /// [`Self::list_visible_images_in_project`].
    async fn list_images_in_silo(&self, silo_id: Uuid) -> Result<Vec<Image>, StoreError>;

    /// List every image whose scope is exactly `Tenant {
    /// tenant_id }`.
    async fn list_images_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Image>, StoreError>;

    /// List every image whose scope is exactly `Project {
    /// project_id }`.
    async fn list_images_in_project(&self, project_id: Uuid) -> Result<Vec<Image>, StoreError>;

    /// List every image whose scope is exactly `User { user_id }`.
    async fn list_images_for_user(&self, user_id: Uuid) -> Result<Vec<Image>, StoreError>;

    /// List every image visible at a tenant URL: Public + Silo
    /// (of tenant's silo) + Tenant. Used by `GET
    /// /v2/tenants/{tenant}/images` for the practical "what can a
    /// tenant member launch from?" query.
    async fn list_visible_images_in_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<Image>, StoreError>;

    /// List every image visible at a project URL: Public + Silo
    /// (of project's silo) + Tenant (of project's tenant) +
    /// Project. Used by `GET
    /// /v2/tenants/{tenant}/projects/{project}/images` for the
    /// practical "what can a project member launch from?" query.
    async fn list_visible_images_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Image>, StoreError>;

    /// Delete an image by id. Visibility / ownership gating is
    /// applied at the handler layer; the store layer just
    /// removes the record.
    async fn delete_image(&self, image_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Project quotas (singleton per project)
    // ------------------------------------------------------------------

    /// Set (or replace) a project's quota record. Returns
    /// [`StoreError::NotFound`] if the project does not exist or
    /// does not live in the supplied tenant (cross-tenant probe
    /// invariant).
    async fn put_quota(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewQuota,
    ) -> Result<Quota, StoreError>;

    /// Read a project's quota. Returns [`StoreError::NotFound`] if
    /// the project does not exist, lives in a different tenant, or
    /// has no quota set.
    async fn get_quota(&self, tenant_id: Uuid, project_id: Uuid) -> Result<Quota, StoreError>;

    /// Remove a project's quota (project becomes unlimited). Returns
    /// [`StoreError::NotFound`] if no quota was set.
    async fn delete_quota(&self, tenant_id: Uuid, project_id: Uuid) -> Result<(), StoreError>;

    // ------------------------------------------------------------------
    // Instances (project-scoped, with lifecycle state machine)
    // ------------------------------------------------------------------

    /// Create an instance, atomically creating its primary NIC and
    /// boot Disk.
    ///
    /// The store enforces structural invariants:
    ///
    /// * Project exists and `project.tenant_id == tenant_id`. Mismatch
    ///   surfaces as [`StoreError::NotFound`] (cross-tenant probe
    ///   story).
    /// * Image exists. The handler layer applies the visibility
    ///   predicate (see `image_visible_to` in the `tritond`
    ///   crate); a referenced image the principal cannot see
    ///   surfaces as [`StoreError::NotFound`] from the handler.
    ///   The store itself does not gate on visibility — the
    ///   handler resolves it before invoking `create_instance`.
    /// * Subnet exists and lives in this project (i.e.
    ///   `subnet.tenant_id == tenant_id` and `subnet.project_id ==
    ///   project_id`). Otherwise [`StoreError::NotFound`].
    /// * Each `ssh_key_id` exists and lives in the silo derived from
    ///   the tenant. Otherwise [`StoreError::NotFound`]. (SSH keys
    ///   are still silo-scoped in E-3; G will move them.)
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
        tenant_id: Uuid,
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

    /// Persist the SmartOS CN that owns this instance's host-side VM.
    ///
    /// This is separate from [`Store::create_instance`] so API handlers
    /// can keep placement policy out of the store transaction. A
    /// replacement clears the old host index before writing the new one.
    /// `None` clears placement and is used only by tests and repair tools.
    async fn set_instance_host_cn(
        &self,
        instance_id: Uuid,
        host_cn_uuid: Option<Uuid>,
    ) -> Result<Instance, StoreError>;

    /// List instances assigned to a SmartOS CN. Used by the M1 tenant
    /// placer to spread new VMs across eligible hosts.
    async fn list_instances_for_cn(&self, host_cn_uuid: Uuid) -> Result<Vec<Instance>, StoreError>;

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
    /// no such NIC exists. Handlers add tenant + project + instance
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
    /// no such Disk exists. Handlers add tenant + project + instance
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
    /// * The project exists and `project.tenant_id == tenant_id`.
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
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewFloatingIp,
    ) -> Result<FloatingIp, StoreError>;

    /// Look up a FloatingIp by id. Handlers add tenant + project
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
    /// tenant + project as the FloatingIp; mismatch surfaces as
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
    /// [`StoreError::NotFound`] if the queue has no Pending jobs
    /// matching the claimer.
    ///
    /// `claimed_by` is a free-form identifier the agent picks
    /// (e.g. `"stub-provisioner"` for the in-process stub).
    ///
    /// `claimer_cn` is the bound CN of the claiming API key, if
    /// any. Job-targeting matrix:
    /// * `claimer_cn = None` → only claims jobs whose
    ///   `target_cn_uuid` is also `None` (the in-process stub
    ///   and other unbound claimers see only unrouted work).
    /// * `claimer_cn = Some(uuid)` → claims jobs whose
    ///   `target_cn_uuid` is `None` or `Some(uuid)`.
    async fn claim_next_job(
        &self,
        claimed_by: &str,
        claimer_cn: Option<Uuid>,
    ) -> Result<ProvisioningJob, StoreError>;

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

    // ------------------------------------------------------------------
    // Compute nodes (CN registration + approval)
    // ------------------------------------------------------------------

    /// Self-register a compute node by `server_uuid`.
    ///
    /// Upsert semantics:
    /// * If no record exists, creates one in [`CnState::Pending`]
    ///   with a fresh `claim_code`, `poll_token`, and the supplied
    ///   `sysinfo`/`hostname`/`admin_ip`. If a global
    ///   [`AutoApproveWindow`] is open and has a remaining slot,
    ///   the record is created directly in [`CnState::Approved`]
    ///   without a claim code; the slot is consumed atomically and
    ///   `bound_api_key_id` is left `None` (the caller mints the
    ///   key and follows up with [`approve_cn_post_register`]).
    /// * If a Pending record already exists, refreshes the
    ///   sysinfo/hostname/admin_ip plus rotates the `claim_code` and
    ///   `poll_token` (the previous code becomes invalid). This is
    ///   the "agent restarted before approval came in" path.
    /// * If an Approved record exists, the call is idempotent —
    ///   sysinfo and `last_seen` are refreshed but credentials are
    ///   not re-minted; the agent is expected to already hold its
    ///   bound API key.
    /// * If a Disabled record exists, returns
    ///   [`StoreError::Conflict`]: an operator must remove the
    ///   record before the same `server_uuid` can re-join.
    ///
    /// Returns the resulting [`Cn`].
    async fn register_cn(
        &self,
        server_uuid: Uuid,
        hostname: String,
        admin_ip: Option<std::net::Ipv4Addr>,
        sysinfo: serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<Cn, StoreError>;

    /// Look up a CN by `server_uuid`.
    async fn get_cn(&self, server_uuid: Uuid) -> Result<Cn, StoreError>;

    /// Look up a CN by its long-poll token. Used by
    /// `GET /v2/agent/register/status` to resolve "the agent
    /// holding this token" to its record. Returns
    /// [`StoreError::NotFound`] if no record matches.
    async fn get_cn_by_poll_token(&self, poll_token: &str) -> Result<Cn, StoreError>;

    /// Look up the Pending CN whose normalized `claim_code` matches.
    /// Returns [`StoreError::NotFound`] for no-such-code OR for a
    /// code whose record is not Pending OR for a code whose
    /// `claim_code_expires_at` has passed (the latter conflated
    /// into NotFound so probes can't distinguish).
    async fn get_cn_by_claim_code(&self, code: &str) -> Result<Cn, StoreError>;

    /// List CNs, optionally filtered by state. Order is unspecified.
    async fn list_cns(&self, state_filter: Option<CnState>) -> Result<Vec<Cn>, StoreError>;

    /// Set the operator-controlled placement role for a CN.
    ///
    /// Returns [`StoreError::NotFound`] if the CN does not exist.
    async fn set_cn_role(&self, server_uuid: Uuid, role: CnRole) -> Result<Cn, StoreError>;

    /// Atomically attach a freshly-minted bound API key to a CN.
    ///
    /// Two callers:
    /// 1. The operator approval flow: CN is Pending; this flips
    ///    state to Approved, clears the claim code, and stashes the
    ///    plaintext credential for the agent's long-poll.
    /// 2. The auto-approve flow: `register_cn` has already created
    ///    the record in Approved state (claim code never issued);
    ///    this attaches the freshly-minted key + plaintext.
    ///
    /// Precondition: `bound_api_key_id.is_none()` AND state is not
    /// Disabled. A second call (after the agent has already retrieved
    /// the credential) would either see a populated `bound_api_key_id`
    /// (returns `Conflict`) or a Disabled record (returns `NotFound`).
    ///
    /// Returns [`StoreError::NotFound`] if the CN does not exist or
    /// is Disabled; [`StoreError::Conflict`] if a credential is
    /// already attached (programmer error — never reapprove).
    async fn approve_cn(
        &self,
        server_uuid: Uuid,
        bound_api_key_id: Uuid,
        pending_credential: String,
        approved_at: DateTime<Utc>,
    ) -> Result<Cn, StoreError>;

    /// Atomically take the pending plaintext credential off a Cn
    /// record. Returns `Ok(None)` when the field was already empty
    /// (idempotent for repeat-poll behavior). Looking up by
    /// `poll_token` rather than `server_uuid` is deliberate: the
    /// agent only has the poll token at this point.
    async fn consume_cn_pending_credential(
        &self,
        poll_token: &str,
    ) -> Result<Option<String>, StoreError>;

    /// Disable a CN (Approved → Disabled or Pending → Disabled).
    /// The bound API key (if any) should be deleted by the caller
    /// in the same logical operation; this method only flips state
    /// and returns the updated record.
    async fn disable_cn(&self, server_uuid: Uuid) -> Result<Cn, StoreError>;

    /// Update a CN's `last_seen` timestamp. Used by the
    /// heartbeater's lightweight ping endpoint.
    async fn update_cn_last_seen(
        &self,
        server_uuid: Uuid,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    /// Replace the agent-published status payload on a CN's
    /// record and bump `last_seen`. Used by the heartbeater's
    /// full status endpoint.
    ///
    /// The payload is opaque to the store — agents pick the
    /// shape. Returns [`StoreError::NotFound`] if the CN does
    /// not exist.
    async fn update_cn_status(
        &self,
        server_uuid: Uuid,
        payload: serde_json::Value,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    // ---- Auto-approve window (singleton) ----

    /// Read the current auto-approve window, if one is open.
    /// Returns `Ok(None)` when no window has been opened or the
    /// last one was closed/expired.
    async fn get_auto_approve_window(&self) -> Result<Option<AutoApproveWindow>, StoreError>;

    /// Open or replace the auto-approve window. The caller is
    /// responsible for clamping `expires_at - opened_at` to
    /// [`AUTO_APPROVE_WINDOW_MAX`] before calling.
    async fn open_auto_approve_window(&self, w: AutoApproveWindow) -> Result<(), StoreError>;

    /// Close the auto-approve window (operator-initiated). Idempotent
    /// if no window is open.
    async fn close_auto_approve_window(&self) -> Result<(), StoreError>;

    /// Atomically: if a window is open, has not expired (`now <
    /// expires_at`), and has a remaining count > 0 (or `None` =
    /// unlimited), decrement remaining_count and return
    /// `Ok(true)`. Otherwise return `Ok(false)`. Used by
    /// `register_cn` to decide whether to short-circuit to
    /// Approved.
    async fn try_consume_auto_approve_slot(&self, now: DateTime<Utc>) -> Result<bool, StoreError>;

    // ------------------------------------------------------------------
    // Realized network state (Agent A, Slice H-1)
    // ------------------------------------------------------------------

    /// Record a realization row for `(resource, realizer)`. Upserts
    /// the existing row when `generation >= existing.generation`;
    /// rejects with [`StoreError::Conflict`] when `generation <
    /// existing.generation` (the realizer is reporting a stale
    /// generation, the "backward report" case the Agent C contract
    /// calls out).
    ///
    /// Idempotent at the same generation: re-writing the same
    /// `(status, message)` is a no-op other than `last_reported_at`.
    /// Status downgrades at the same generation are allowed (the
    /// dataplane could subsequently fail at a previously-applied
    /// generation due to a transient issue).
    ///
    /// The store does not validate that `resource.id()` actually
    /// points at an existing record; that check is enforced at the
    /// API edge by Slice H-13 so the dispatch can return a clean
    /// 404 before falling through to this method.
    async fn record_network_realization(
        &self,
        resource: NetworkResourceId,
        realizer: RealizerId,
        generation: u64,
        status: RealizationStatus,
        message: Option<String>,
    ) -> Result<(), StoreError>;

    /// Read every per-realizer row for `resource`, sorted by
    /// `(realizer.kind_tag(), realizer.id())`. Returns an empty
    /// vector when no realizer has reported yet — this is the
    /// normal pre-realization state, *not* a not-found condition.
    /// Callers project the rows into a [`RealizedNetworkState`] view
    /// via [`RealizedNetworkState::from_rows`].
    async fn list_network_realizations(
        &self,
        resource: NetworkResourceId,
    ) -> Result<Vec<Realization>, StoreError>;
}
