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
//! user/by_federation/<tenant>/<sha256>
//!                                   -> uuid hyphenated bytes
//! apikey/by_id/<uuid>               -> JSON-encoded ApiKey
//! apikey/by_lookup/<lookup_id>      -> uuid hyphenated bytes
//! apikey/by_user/<uuid>/<key-uuid>  -> empty (membership index)
//! idp/by_tenant/<uuid>              -> JSON-encoded IdpConfig
//! idp/by_issuer/<sha256-hex>        -> tenant uuid hyphenated bytes
//!                                      (issuer-uniqueness reverse index;
//!                                       sha256 keeps URLs out of the keyspace)
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
//! route_table/by_id/<uuid>          -> JSON-encoded RouteTable
//! route_table/by_vpc/<vpc>/<name>   -> uuid hyphenated bytes
//! route_table/in_vpc/<vpc>/<rt>     -> empty (membership index)
//! route_table/main/<vpc>            -> uuid hyphenated bytes
//! route/by_id/<uuid>                -> JSON-encoded Route
//! route/by_table/<rt>/<destination> -> uuid hyphenated bytes
//! route/in_table/<rt>/<route>       -> empty (membership index)
//! nat_gateway/by_id/<uuid>          -> JSON-encoded NatGatewayRecord
//! nat_gateway/by_vpc/<vpc>/<name>   -> uuid hyphenated bytes
//! nat_gateway/in_vpc/<vpc>/<nat>    -> empty (membership index)
//! edge_cluster/by_id/<uuid>         -> JSON-encoded EdgeClusterRecord
//! edge_cluster/by_name/<name>        -> uuid hyphenated bytes
//! edge_cluster/all/<uuid>           -> empty (membership index)
//! edge_cluster/by_resource/<kind>/<resource>/<edge>
//!                                   -> empty (reverse resource index)
//! ssh_key/by_id/<uuid>              -> JSON-encoded SshKey
//! ssh_key/by_public/<name>          -> uuid hyphenated bytes
//! ssh_key/by_silo/<silo>/<name>     -> uuid hyphenated bytes
//! ssh_key/by_tenant/<tenant>/<name> -> uuid hyphenated bytes
//! ssh_key/by_project/<proj>/<name>  -> uuid hyphenated bytes
//! ssh_key/by_user/<user>/<name>     -> uuid hyphenated bytes
//! ssh_key/by_public_fp/<fp>         -> uuid hyphenated bytes
//! ssh_key/by_silo_fp/<silo>/<fp>    -> uuid hyphenated bytes
//! ssh_key/by_tenant_fp/<tenant>/<fp>
//!                                   -> uuid hyphenated bytes
//! ssh_key/by_project_fp/<proj>/<fp> -> uuid hyphenated bytes
//! ssh_key/by_user_fp/<user>/<fp>    -> uuid hyphenated bytes
//! ssh_key/in_public/<key>           -> empty (membership index, public)
//! ssh_key/in_silo/<silo>/<key>      -> empty (membership index, silo)
//! ssh_key/in_tenant/<tenant>/<key>  -> empty (membership index, tenant)
//! ssh_key/in_project/<proj>/<key>   -> empty (membership index, project)
//! ssh_key/by_user_idx/<user>/<key>  -> empty (membership index, user)
//! image/by_id/<uuid>                -> JSON-encoded Image
//! image/by_public/<name>            -> uuid hyphenated bytes
//! image/by_silo/<silo>/<name>       -> uuid hyphenated bytes
//! image/by_tenant/<tenant>/<name>   -> uuid hyphenated bytes
//! image/by_project/<proj>/<name>    -> uuid hyphenated bytes
//! image/by_user/<user>/<name>       -> uuid hyphenated bytes
//! image/in_public/<image>           -> empty (membership index, public scope)
//! image/in_silo/<silo>/<image>      -> empty (membership index, silo scope)
//! image/in_tenant/<tenant>/<image>  -> empty (membership index, tenant scope)
//! image/in_project/<proj>/<image>   -> empty (membership index, project scope)
//! image/by_user_idx/<user>/<image>  -> empty (membership index, user scope)
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
//! floating_ip/alloc/v4/<addr>       -> holder bytes, e.g. floating_ip:<uuid>
//!                                      or nat_gateway:<uuid>
//! floating_ip/alloc/v6/<addr>       -> holder bytes, e.g. floating_ip:<uuid>
//!                                      or nat_gateway:<uuid>
//! job/by_id/<uuid>                  -> JSON-encoded ProvisioningJob
//! job/pending/<seq-be-u64>          -> uuid hyphenated bytes (FIFO queue)
//! job/seq/counter                   -> next seq, big-endian u64
//! cn/by_uuid/<server_uuid>          -> JSON-encoded Cn
//! cn/by_claim/<normalized_code>     -> server_uuid hyphenated bytes
//! cn/by_poll/<poll_token>           -> server_uuid hyphenated bytes
//! cn/by_state/<state>/<server_uuid> -> empty (membership index)
//! auto_approve/window               -> JSON-encoded AutoApproveWindow (singleton)
//! config/settings                   -> JSON-encoded Settings (singleton; absent
//!                                      key means "all defaults")
//! system/<tag>                      -> raw bytes (e.g. JWT signing key)
//! network_realization/<kind>/<resource_id>/<realizer_kind>/<realizer_id>
//!                                   -> JSON-encoded Realization
//!                                      (Slice H-1; <kind> matches the
//!                                       NetworkResourceId serde wire tag,
//!                                       <realizer_kind> matches the
//!                                       RealizerId serde wire tag —
//!                                       both kept in lockstep with
//!                                       `NetworkResourceId::kind_tag` and
//!                                       `RealizerId::kind_tag`.)
//! dhcp_pool/by_vpc/<vpc>            -> JSON-encoded DhcpPool (singleton per VPC)
//! dhcp_reservation/by_vpc/<vpc>/<mac>
//!                                   -> JSON-encoded DhcpReservation (mac is
//!                                      canonical lowercase colon form;
//!                                      list-by-vpc range-scans the
//!                                      `dhcp_reservation/by_vpc/<vpc>/`
//!                                      prefix)
//! dhcp_lease/by_vpc/<vpc>/<mac>     -> JSON-encoded DhcpLease (same shape;
//!                                      `list_all_dhcp_leases` range-scans
//!                                      `dhcp_lease/by_vpc/`)
//! storage_cluster/by_id/<uuid>      -> JSON-encoded StorageCluster
//! storage_cluster/by_name/<name>    -> uuid hyphenated bytes
//! storage_cluster/all/<uuid>        -> empty (membership index)
//! ```
//!
//! Each multi-key write happens in a single transaction so name
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
use crate::{
    AddressFamily, ApiKey, AutoApproveWindow, CLAIM_CODE_TTL, Cn, CnRole, CnState, DhcpLease,
    DhcpPool, DhcpReservation, Disk, DiskKind, EdgeCluster, EdgeClusterKind, EdgeClusterResource,
    FLOATING_IP_V4_POOL, FLOATING_IP_V6_POOL, FirewallRule, FloatingIp, FloatingIpAttachment,
    IdpConfig, Image, ImageScope, Instance, InstanceBrand, InstanceCreateResult, JobOutcome,
    JobStatus, JobStatusKind, LegacyVm, LifecycleState, LifecycleStateKind, NatGateway,
    NetworkResourceId, NewDhcpPool, NewDhcpReservation, NewEdgeCluster, NewFirewallRule,
    NewFloatingIp, NewImage, NewInstance, NewJob, NewNatGateway, NewProject, NewQuota, NewRoute,
    NewRouteTable, NewSilo, NewSshKey, NewStorageCluster, NewSubnet, NewTenant, NewVpc, Nic,
    Project, ProvisioningJob, Quota, Realization, RealizationStatus, RealizerId, Route, RouteTable,
    RouteTarget, Settings, Silo, SshKey, SshKeyScope, StorageCluster, StorageClusterStatus, Store,
    StoreError, Subnet, SystemKey, Tenant, User, VPC_VNI_MAX, VPC_VNI_RESERVED_CEILING, Vpc,
    default_boot_disk_size_bytes, generate_claim_code, generate_poll_token,
};

/// Maximum attempts to draw a fresh VNI before giving up. Mirrors the
/// in-memory store's cap; with ~16.7M candidates this is operationally
/// unreachable.
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

    fn user_federation_key(tenant_id: Uuid, issuer: &str, subject: &str) -> Vec<u8> {
        // SHA-256 of `issuer\0subject` → fixed-length, no escaping
        // worries for arbitrary issuer URLs that contain slashes.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(issuer.as_bytes());
        hasher.update(b"\0");
        hasher.update(subject.as_bytes());
        let digest = hasher.finalize();
        let hex = digest_to_hex(&digest);
        format!("user/by_federation/{tenant_id}/{hex}").into_bytes()
    }

    fn idp_config_key(tenant_id: Uuid) -> Vec<u8> {
        format!("idp/by_tenant/{tenant_id}").into_bytes()
    }

    fn idp_config_prefix() -> &'static [u8] {
        b"idp/by_tenant/"
    }

    /// Reverse index: SHA-256(issuer) → owning tenant uuid. Hashing
    /// keeps arbitrary issuer URLs (slashes, ports, paths) out of
    /// the key space the same way ssh-key fingerprint indices do.
    fn idp_by_issuer_key(issuer: &str) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(issuer.as_bytes());
        let digest = hasher.finalize();
        let hex = digest_to_hex(&digest);
        format!("idp/by_issuer/{hex}").into_bytes()
    }

    fn project_by_id_key(id: Uuid) -> Vec<u8> {
        format!("project/by_id/{id}").into_bytes()
    }

    fn project_by_tenant_name_key(tenant_id: Uuid, name: &str) -> Vec<u8> {
        format!("project/by_tenant/{tenant_id}/{name}").into_bytes()
    }

    fn project_in_tenant_key(tenant_id: Uuid, project_id: Uuid) -> Vec<u8> {
        format!("project/in_tenant/{tenant_id}/{project_id}").into_bytes()
    }

    fn project_in_tenant_prefix(tenant_id: Uuid) -> Vec<u8> {
        format!("project/in_tenant/{tenant_id}/").into_bytes()
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

    fn route_table_by_id_key(id: Uuid) -> Vec<u8> {
        format!("route_table/by_id/{id}").into_bytes()
    }

    fn route_table_by_vpc_name_key(vpc_id: Uuid, name: &str) -> Vec<u8> {
        format!("route_table/by_vpc/{vpc_id}/{name}").into_bytes()
    }

    fn route_table_in_vpc_key(vpc_id: Uuid, route_table_id: Uuid) -> Vec<u8> {
        format!("route_table/in_vpc/{vpc_id}/{route_table_id}").into_bytes()
    }

    fn route_table_in_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
        format!("route_table/in_vpc/{vpc_id}/").into_bytes()
    }

    fn route_table_main_key(vpc_id: Uuid) -> Vec<u8> {
        format!("route_table/main/{vpc_id}").into_bytes()
    }

    fn route_by_id_key(id: Uuid) -> Vec<u8> {
        format!("route/by_id/{id}").into_bytes()
    }

    fn route_by_id_prefix() -> Vec<u8> {
        b"route/by_id/".to_vec()
    }

    fn route_by_table_destination_key(route_table_id: Uuid, destination: IpNetwork) -> Vec<u8> {
        format!("route/by_table/{route_table_id}/{destination}").into_bytes()
    }

    fn route_in_table_key(route_table_id: Uuid, route_id: Uuid) -> Vec<u8> {
        format!("route/in_table/{route_table_id}/{route_id}").into_bytes()
    }

    fn route_in_table_prefix(route_table_id: Uuid) -> Vec<u8> {
        format!("route/in_table/{route_table_id}/").into_bytes()
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

    fn nat_gateway_by_id_key(id: Uuid) -> Vec<u8> {
        format!("nat_gateway/by_id/{id}").into_bytes()
    }

    fn nat_gateway_by_vpc_name_key(vpc_id: Uuid, name: &str) -> Vec<u8> {
        format!("nat_gateway/by_vpc/{vpc_id}/{name}").into_bytes()
    }

    fn nat_gateway_in_vpc_key(vpc_id: Uuid, nat_gateway_id: Uuid) -> Vec<u8> {
        format!("nat_gateway/in_vpc/{vpc_id}/{nat_gateway_id}").into_bytes()
    }

    fn nat_gateway_in_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
        format!("nat_gateway/in_vpc/{vpc_id}/").into_bytes()
    }

    fn edge_cluster_by_id_key(id: Uuid) -> Vec<u8> {
        format!("edge_cluster/by_id/{id}").into_bytes()
    }

    fn edge_cluster_by_name_key(name: &str) -> Vec<u8> {
        format!("edge_cluster/by_name/{name}").into_bytes()
    }

    fn edge_cluster_all_key(id: Uuid) -> Vec<u8> {
        format!("edge_cluster/all/{id}").into_bytes()
    }

    fn edge_cluster_all_prefix() -> &'static [u8] {
        b"edge_cluster/all/"
    }

    fn edge_cluster_by_resource_key(resource: EdgeClusterResource, id: Uuid) -> Vec<u8> {
        format!(
            "edge_cluster/by_resource/{}/{}/{id}",
            resource.kind_tag(),
            resource.id()
        )
        .into_bytes()
    }

    fn edge_cluster_by_resource_prefix(resource: EdgeClusterResource) -> Vec<u8> {
        format!(
            "edge_cluster/by_resource/{}/{}/",
            resource.kind_tag(),
            resource.id()
        )
        .into_bytes()
    }

    fn storage_cluster_by_id_key(id: Uuid) -> Vec<u8> {
        format!("storage_cluster/by_id/{id}").into_bytes()
    }

    fn storage_cluster_by_name_key(name: &str) -> Vec<u8> {
        format!("storage_cluster/by_name/{name}").into_bytes()
    }

    fn storage_cluster_all_key(id: Uuid) -> Vec<u8> {
        format!("storage_cluster/all/{id}").into_bytes()
    }

    fn storage_cluster_all_prefix() -> &'static [u8] {
        b"storage_cluster/all/"
    }

    fn ssh_key_by_id_key(id: Uuid) -> Vec<u8> {
        format!("ssh_key/by_id/{id}").into_bytes()
    }

    fn ssh_key_by_public_name_key(name: &str) -> Vec<u8> {
        format!("ssh_key/by_public/{name}").into_bytes()
    }

    fn ssh_key_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
        format!("ssh_key/by_silo/{silo_id}/{name}").into_bytes()
    }

    fn ssh_key_by_tenant_name_key(tenant_id: Uuid, name: &str) -> Vec<u8> {
        format!("ssh_key/by_tenant/{tenant_id}/{name}").into_bytes()
    }

    fn ssh_key_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
        format!("ssh_key/by_project/{project_id}/{name}").into_bytes()
    }

    fn ssh_key_by_user_name_key(user_id: Uuid, name: &str) -> Vec<u8> {
        format!("ssh_key/by_user/{user_id}/{name}").into_bytes()
    }

    fn ssh_key_by_public_fp_key(fingerprint: &str) -> Vec<u8> {
        format!("ssh_key/by_public_fp/{fingerprint}").into_bytes()
    }

    fn ssh_key_by_silo_fp_key(silo_id: Uuid, fingerprint: &str) -> Vec<u8> {
        format!("ssh_key/by_silo_fp/{silo_id}/{fingerprint}").into_bytes()
    }

    fn ssh_key_by_tenant_fp_key(tenant_id: Uuid, fingerprint: &str) -> Vec<u8> {
        format!("ssh_key/by_tenant_fp/{tenant_id}/{fingerprint}").into_bytes()
    }

    fn ssh_key_by_project_fp_key(project_id: Uuid, fingerprint: &str) -> Vec<u8> {
        format!("ssh_key/by_project_fp/{project_id}/{fingerprint}").into_bytes()
    }

    fn ssh_key_by_user_fp_key(user_id: Uuid, fingerprint: &str) -> Vec<u8> {
        format!("ssh_key/by_user_fp/{user_id}/{fingerprint}").into_bytes()
    }

    fn ssh_key_in_public_key(key_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_public/{key_id}").into_bytes()
    }

    fn ssh_key_in_public_prefix() -> Vec<u8> {
        b"ssh_key/in_public/".to_vec()
    }

    fn ssh_key_in_silo_key(silo_id: Uuid, key_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_silo/{silo_id}/{key_id}").into_bytes()
    }

    fn ssh_key_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_silo/{silo_id}/").into_bytes()
    }

    fn ssh_key_in_tenant_key(tenant_id: Uuid, key_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_tenant/{tenant_id}/{key_id}").into_bytes()
    }

    fn ssh_key_in_tenant_prefix(tenant_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_tenant/{tenant_id}/").into_bytes()
    }

    fn ssh_key_in_project_key(project_id: Uuid, key_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_project/{project_id}/{key_id}").into_bytes()
    }

    fn ssh_key_in_project_prefix(project_id: Uuid) -> Vec<u8> {
        format!("ssh_key/in_project/{project_id}/").into_bytes()
    }

    fn ssh_key_by_user_idx_key(user_id: Uuid, key_id: Uuid) -> Vec<u8> {
        format!("ssh_key/by_user_idx/{user_id}/{key_id}").into_bytes()
    }

    fn ssh_key_by_user_idx_prefix(user_id: Uuid) -> Vec<u8> {
        format!("ssh_key/by_user_idx/{user_id}/").into_bytes()
    }

    fn image_by_id_key(id: Uuid) -> Vec<u8> {
        format!("image/by_id/{id}").into_bytes()
    }

    fn image_by_public_name_key(name: &str) -> Vec<u8> {
        format!("image/by_public/{name}").into_bytes()
    }

    fn image_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
        format!("image/by_silo/{silo_id}/{name}").into_bytes()
    }

    fn image_by_tenant_name_key(tenant_id: Uuid, name: &str) -> Vec<u8> {
        format!("image/by_tenant/{tenant_id}/{name}").into_bytes()
    }

    fn image_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
        format!("image/by_project/{project_id}/{name}").into_bytes()
    }

    fn image_by_user_name_key(user_id: Uuid, name: &str) -> Vec<u8> {
        format!("image/by_user/{user_id}/{name}").into_bytes()
    }

    fn image_in_public_key(image_id: Uuid) -> Vec<u8> {
        format!("image/in_public/{image_id}").into_bytes()
    }

    fn image_in_public_prefix() -> Vec<u8> {
        b"image/in_public/".to_vec()
    }

    fn image_in_silo_key(silo_id: Uuid, image_id: Uuid) -> Vec<u8> {
        format!("image/in_silo/{silo_id}/{image_id}").into_bytes()
    }

    fn image_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
        format!("image/in_silo/{silo_id}/").into_bytes()
    }

    fn image_in_tenant_key(tenant_id: Uuid, image_id: Uuid) -> Vec<u8> {
        format!("image/in_tenant/{tenant_id}/{image_id}").into_bytes()
    }

    fn image_in_tenant_prefix(tenant_id: Uuid) -> Vec<u8> {
        format!("image/in_tenant/{tenant_id}/").into_bytes()
    }

    fn image_in_project_key(project_id: Uuid, image_id: Uuid) -> Vec<u8> {
        format!("image/in_project/{project_id}/{image_id}").into_bytes()
    }

    fn image_in_project_prefix(project_id: Uuid) -> Vec<u8> {
        format!("image/in_project/{project_id}/").into_bytes()
    }

    fn image_by_user_idx_key(user_id: Uuid, image_id: Uuid) -> Vec<u8> {
        format!("image/by_user_idx/{user_id}/{image_id}").into_bytes()
    }

    fn image_by_user_idx_prefix(user_id: Uuid) -> Vec<u8> {
        format!("image/by_user_idx/{user_id}/").into_bytes()
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

    fn instance_in_host_cn_key(host_cn_uuid: Uuid, instance_id: Uuid) -> Vec<u8> {
        format!("instance/in_host_cn/{host_cn_uuid}/{instance_id}").into_bytes()
    }

    fn instance_in_host_cn_prefix(host_cn_uuid: Uuid) -> Vec<u8> {
        format!("instance/in_host_cn/{host_cn_uuid}/").into_bytes()
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

    fn public_ip_holder_value(resource: NetworkResourceId) -> Vec<u8> {
        format!("{}:{}", resource.kind_tag(), resource.id()).into_bytes()
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

    fn settings_key() -> &'static [u8] {
        b"config/settings"
    }

    fn legacy_vm_by_id_key(smartos_uuid: Uuid) -> Vec<u8> {
        format!("legacy_vm/by_id/{smartos_uuid}").into_bytes()
    }

    fn legacy_vm_in_host_cn_key(host_cn_uuid: Uuid, smartos_uuid: Uuid) -> Vec<u8> {
        format!("legacy_vm/in_host_cn/{host_cn_uuid}/{smartos_uuid}").into_bytes()
    }

    fn legacy_vm_in_host_cn_prefix(host_cn_uuid: Uuid) -> Vec<u8> {
        format!("legacy_vm/in_host_cn/{host_cn_uuid}/").into_bytes()
    }

    fn legacy_vm_by_id_prefix() -> &'static [u8] {
        b"legacy_vm/by_id/"
    }

    /// Key for one realization row. `network_realization/<kind>/<resource_id>/<realizer_kind>/<realizer_id>`.
    fn network_realization_key(resource: NetworkResourceId, realizer: RealizerId) -> Vec<u8> {
        format!(
            "network_realization/{}/{}/{}/{}",
            resource.kind_tag(),
            resource.id(),
            realizer.kind_tag(),
            realizer.id(),
        )
        .into_bytes()
    }

    /// Prefix scan for every realizer's row on a given resource.
    fn network_realization_resource_prefix(resource: NetworkResourceId) -> Vec<u8> {
        format!(
            "network_realization/{}/{}/",
            resource.kind_tag(),
            resource.id(),
        )
        .into_bytes()
    }

    fn dhcp_pool_by_vpc_key(vpc_id: Uuid) -> Vec<u8> {
        format!("dhcp_pool/by_vpc/{vpc_id}").into_bytes()
    }

    fn dhcp_reservation_by_vpc_mac_key(vpc_id: Uuid, mac: &str) -> Vec<u8> {
        format!("dhcp_reservation/by_vpc/{vpc_id}/{mac}").into_bytes()
    }

    fn dhcp_reservation_by_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
        format!("dhcp_reservation/by_vpc/{vpc_id}/").into_bytes()
    }

    fn dhcp_lease_by_vpc_mac_key(vpc_id: Uuid, mac: &str) -> Vec<u8> {
        format!("dhcp_lease/by_vpc/{vpc_id}/{mac}").into_bytes()
    }

    fn dhcp_lease_by_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
        format!("dhcp_lease/by_vpc/{vpc_id}/").into_bytes()
    }

    fn dhcp_lease_global_prefix() -> &'static [u8] {
        b"dhcp_lease/by_vpc/"
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
        // Federation index is keyed by (tenant_id, issuer, subject) —
        // post E-5 the IdP is tenant-scoped, so the index is rooted
        // directly at the tenant. The defensive tenant existence
        // check still happens (a federated user without a tenant is
        // a programming error).
        let federation_key = match (user.tenant_id, user.federation.as_ref()) {
            (Some(tenant_id), Some(fed)) => {
                // Confirm the tenant exists; fail clean otherwise.
                let _ = self.get_tenant(tenant_id).await?;
                Some(Self::user_federation_key(
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

    async fn update_user_password_hash(
        &self,
        username: &str,
        password_hash: String,
    ) -> Result<User, StoreError> {
        let by_name_key = Self::user_by_name_key(username);
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
                    let id_str = std::str::from_utf8(id_bytes.as_ref()).map_err(|e| {
                        FdbBindingError::CustomError(
                            format!("user index value not utf8: {e}").into(),
                        )
                    })?;
                    let id = Uuid::parse_str(id_str).map_err(|e| {
                        FdbBindingError::CustomError(
                            format!("user index value not uuid: {e}").into(),
                        )
                    })?;
                    let by_id_key = Self::user_by_id_key(id);
                    let Some(user_bytes) = tr.get(&by_id_key, false).await? else {
                        return Ok(None);
                    };
                    let mut user: User =
                        serde_json::from_slice(user_bytes.as_ref()).map_err(|e| {
                            FdbBindingError::CustomError(format!("deserialize user: {e}").into())
                        })?;
                    if user.username != username {
                        return Ok(None);
                    }
                    user.password_hash = password_hash;
                    let value = serde_json::to_vec(&user).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize user: {e}").into())
                    })?;
                    tr.set(&by_id_key, &value);
                    Ok(Some(user))
                }
            })
            .await;

        match result {
            Ok(Some(user)) => Ok(user),
            Ok(None) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
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

    async fn get_settings(&self) -> Result<Settings, StoreError> {
        let key = Self::settings_key().to_vec();
        match self.read_bytes(&key).await? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Backend(format!("deserialize settings: {e}"))),
            None => Ok(Settings::default()),
        }
    }

    async fn put_settings(&self, settings: Settings) -> Result<(), StoreError> {
        let value = serde_json::to_vec(&settings)
            .map_err(|e| StoreError::Backend(format!("serialize settings: {e}")))?;
        let key = Self::settings_key().to_vec();
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
        tenant_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<User, StoreError> {
        let federation_key = Self::user_federation_key(tenant_id, issuer, subject);
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
        let by_tenant_key = Self::idp_config_key(tenant_id);
        let by_issuer_key = Self::idp_by_issuer_key(&config.issuer_url);
        let value = serde_json::to_vec(&config)
            .map_err(|e| StoreError::Backend(format!("serialize idp config: {e}")))?;
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
                        let prev: IdpConfig = serde_json::from_slice(&prev_bytes).map_err(|e| {
                            FdbBindingError::CustomError(
                                format!("deserialize prev idp config: {e}").into(),
                            )
                        })?;
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_idp_config(&self, tenant_id: Uuid) -> Result<IdpConfig, StoreError> {
        let key = Self::idp_config_key(tenant_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize idp config: {e}")))
    }

    async fn delete_idp_config(&self, tenant_id: Uuid) -> Result<(), StoreError> {
        let by_tenant_key = Self::idp_config_key(tenant_id);
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
            let tenant_str = std::str::from_utf8(suffix)
                .map_err(|e| StoreError::Backend(format!("idp index key not utf8: {e}")))?;
            let tenant_id = Uuid::parse_str(tenant_str)
                .map_err(|e| StoreError::Backend(format!("idp index key not uuid: {e}")))?;
            let config: IdpConfig = serde_json::from_slice(&value)
                .map_err(|e| StoreError::Backend(format!("deserialize idp config: {e}")))?;
            out.push((tenant_id, config));
        }
        Ok(out)
    }

    async fn get_idp_config_by_issuer(
        &self,
        issuer: &str,
    ) -> Result<(Uuid, IdpConfig), StoreError> {
        let by_issuer_key = Self::idp_by_issuer_key(issuer);
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
        let project = Project {
            id: Uuid::new_v4(),
            tenant_id,
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        let value = serde_json::to_vec(&project)
            .map_err(|e| StoreError::Backend(format!("serialize project: {e}")))?;
        let by_id_key = Self::project_by_id_key(project.id);
        let by_name_key = Self::project_by_tenant_name_key(tenant_id, &project.name);
        let in_tenant_key = Self::project_in_tenant_key(tenant_id, project.id);
        let tenant_check_key = Self::tenant_by_id_key(tenant_id);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_project(&self, project_id: Uuid) -> Result<Project, StoreError> {
        let key = Self::project_by_id_key(project_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize project: {e}")))
    }

    async fn list_projects_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Project>, StoreError> {
        let prefix = Self::project_in_tenant_prefix(tenant_id);
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
        // tenant_id + name to clear from the indices. Concurrent
        // delete shows up as Outcome::Vanished below.
        let by_id_key = Self::project_by_id_key(project_id);
        let bytes = match self.read_bytes(&by_id_key).await? {
            Some(b) => b,
            None => return Err(StoreError::NotFound),
        };
        let project: Project = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize project: {e}")))?;
        let by_name_key = Self::project_by_tenant_name_key(project.tenant_id, &project.name);
        let in_tenant_key = Self::project_in_tenant_key(project.tenant_id, project.id);

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_tenant_key = in_tenant_key.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DelOut::Vanished);
                    }
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
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewVpc,
    ) -> Result<Vpc, StoreError> {
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

        let project_check_key = Self::project_by_id_key(project_id);
        let by_name_key = Self::vpc_by_project_name_key(project_id, &req.name);

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
                .map_err(|e| StoreError::Backend(format!("serialize vpc: {e}")))?;
            let main_route_table_value = serde_json::to_vec(&main_route_table)
                .map_err(|e| StoreError::Backend(format!("serialize route table: {e}")))?;
            let by_id_key = Self::vpc_by_id_key(candidate.id);
            let in_project_key = Self::vpc_in_project_key(project_id, candidate.id);
            let by_vni_key = Self::vpc_by_vni_key(vni);
            let rt_by_id_key = Self::route_table_by_id_key(route_table_id);
            let rt_by_name_key = Self::route_table_by_vpc_name_key(vpc_id, MAIN_ROUTE_TABLE_NAME);
            let rt_in_vpc_key = Self::route_table_in_vpc_key(vpc_id, route_table_id);
            let rt_main_key = Self::route_table_main_key(vpc_id);
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
        let route_table_prefix = Self::route_table_in_vpc_prefix(vpc_id);
        let (route_table_begin, route_table_end) = prefix_range(&route_table_prefix);
        let route_table_prefix_len = route_table_prefix.len();
        let main_route_table_id = vpc.main_route_table_id;
        let main_rt_by_id_key = Self::route_table_by_id_key(main_route_table_id);
        let main_rt_by_name_key = Self::route_table_by_vpc_name_key(vpc_id, MAIN_ROUTE_TABLE_NAME);
        let main_rt_in_vpc_key = Self::route_table_in_vpc_key(vpc_id, main_route_table_id);
        let main_rt_singleton_key = Self::route_table_main_key(vpc_id);

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
                let by_name_key = by_name_key.clone();
                let in_project_key = in_project_key.clone();
                let by_vni_key = by_vni_key.clone();
                let subnet_begin = subnet_begin.clone();
                let subnet_end = subnet_end.clone();
                let route_table_begin = route_table_begin.clone();
                let route_table_end = route_table_end.clone();
                let main_rt_by_id_key = main_rt_by_id_key.clone();
                let main_rt_by_name_key = main_rt_by_name_key.clone();
                let main_rt_in_vpc_key = main_rt_in_vpc_key.clone();
                let main_rt_singleton_key = main_rt_singleton_key.clone();
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
                            && id != main_route_table_id
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_subnet(
        &self,
        tenant_id: Uuid,
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

    async fn create_route_table(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewRouteTable,
    ) -> Result<RouteTable, StoreError> {
        let vpc_check_key = Self::vpc_by_id_key(vpc_id);
        let by_name_key = Self::route_table_by_vpc_name_key(vpc_id, &req.name);
        let route_table_id = Uuid::new_v4();
        let by_id_key = Self::route_table_by_id_key(route_table_id);
        let in_vpc_key = Self::route_table_in_vpc_key(vpc_id, route_table_id);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_route_table(&self, route_table_id: Uuid) -> Result<RouteTable, StoreError> {
        let key = Self::route_table_by_id_key(route_table_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize route table: {e}")))
    }

    async fn list_route_tables_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<RouteTable>, StoreError> {
        let prefix = Self::route_table_in_vpc_prefix(vpc_id);
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
        let by_id_key = Self::route_table_by_id_key(route_table_id);

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

                    let route_prefix = Self::route_in_table_prefix(route_table_id);
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

                    let subnet_prefix = Self::subnet_in_vpc_prefix(route_table.vpc_id);
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
                        let subnet_key = Self::subnet_by_id_key(subnet_id);
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
                        Self::route_table_by_vpc_name_key(route_table.vpc_id, &route_table.name);
                    let in_vpc_key =
                        Self::route_table_in_vpc_key(route_table.vpc_id, route_table.id);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
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
        let destination = crate::types::canonical_ip_network(req.destination);
        let route_table_key = Self::route_table_by_id_key(route_table_id);
        let vpc_key = Self::vpc_by_id_key(vpc_id);
        let by_destination_key = Self::route_by_table_destination_key(route_table_id, destination);
        let route_id = Uuid::new_v4();
        let by_id_key = Self::route_by_id_key(route_id);
        let in_table_key = Self::route_in_table_key(route_table_id, route_id);
        let id_str = route_id.to_string();

        if let RouteTarget::NatGateway { nat_gateway_id } = &req.target {
            let nat = self.read_nat_gateway_record(*nat_gateway_id).await?;
            Self::validate_nat_gateway_route_target(&nat, tenant_id, project_id, vpc_id)?;
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_route(&self, route_id: Uuid) -> Result<Route, StoreError> {
        let key = Self::route_by_id_key(route_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize route: {e}")))
    }

    async fn list_routes_in_table(&self, route_table_id: Uuid) -> Result<Vec<Route>, StoreError> {
        let prefix = Self::route_in_table_prefix(route_table_id);
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
        let by_id_key = Self::route_by_id_key(route_id);

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
                    tr.clear(&Self::route_by_table_destination_key(
                        route.route_table_id,
                        route.destination,
                    ));
                    tr.clear(&Self::route_in_table_key(route.route_table_id, route.id));
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) => Ok(()),
            Ok(Out::Vanished) => Err(StoreError::NotFound),
            Ok(Out::Corrupt(e)) => Err(StoreError::Backend(format!("deserialize route: {e}"))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_nat_gateway(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewNatGateway,
    ) -> Result<NatGateway, StoreError> {
        let vpc_check_key = Self::vpc_by_id_key(vpc_id);
        let by_name_key = Self::nat_gateway_by_vpc_name_key(vpc_id, &req.name);
        let alloc_v4_prefix = Self::floating_ip_alloc_v4_prefix().to_vec();
        let alloc_v6_prefix = Self::floating_ip_alloc_v6_prefix().to_vec();
        let (v4_begin, v4_end) = prefix_range(&alloc_v4_prefix);
        let (v6_begin, v6_end) = prefix_range(&alloc_v6_prefix);
        let v4_prefix_len = alloc_v4_prefix.len();
        let v6_prefix_len = alloc_v6_prefix.len();

        let nat_gateway_id = Uuid::new_v4();
        let by_id_key = Self::nat_gateway_by_id_key(nat_gateway_id);
        let in_vpc_key = Self::nat_gateway_in_vpc_key(vpc_id, nat_gateway_id);
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
                    let holder = Self::public_ip_holder_value(NetworkResourceId::NatGateway {
                        id: nat_gateway_id,
                    });
                    match public_address {
                        std::net::IpAddr::V4(v4) => {
                            tr.set(&Self::floating_ip_alloc_v4_key(v4), &holder);
                        }
                        std::net::IpAddr::V6(v6) => {
                            tr.set(&Self::floating_ip_alloc_v6_key(v6), &holder);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
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
        let prefix = Self::nat_gateway_in_vpc_prefix(vpc_id);
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
        let by_id_key = Self::nat_gateway_by_id_key(nat_gateway_id);

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
                    let route_prefix = Self::route_by_id_prefix();
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
                        Self::nat_gateway_by_vpc_name_key(record.vpc_id, &record.name);
                    let in_vpc_key = Self::nat_gateway_in_vpc_key(record.vpc_id, record.id);
                    tr.clear(&by_id_key);
                    tr.clear(&by_name_key);
                    tr.clear(&in_vpc_key);
                    match record.public_address {
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
            Ok(Out::HasRoutes) => Err(StoreError::Conflict(format!(
                "nat gateway {nat_gateway_id} is still referenced by routes"
            ))),
            Ok(Out::Corrupt(e)) => {
                Err(StoreError::Backend(format!("deserialize nat gateway: {e}")))
            }
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_edge_cluster(&self, req: NewEdgeCluster) -> Result<EdgeCluster, StoreError> {
        validate_edge_cluster_bound_resource_shape(req.kind, &req.bound_resources)?;

        let edge_cluster_id = Uuid::new_v4();
        let by_id_key = Self::edge_cluster_by_id_key(edge_cluster_id);
        let by_name_key = Self::edge_cluster_by_name_key(&req.name);
        let all_key = Self::edge_cluster_all_key(edge_cluster_id);
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
                        let key = Self::edge_cluster_by_resource_key(*resource, record.id);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_edge_cluster(&self, edge_cluster_id: Uuid) -> Result<EdgeCluster, StoreError> {
        let record = self.read_edge_cluster_record(edge_cluster_id).await?;
        let rows = self.list_network_realizations(record.resource_id()).await?;
        Ok(record.into_view(rows))
    }

    async fn list_edge_clusters(&self) -> Result<Vec<EdgeCluster>, StoreError> {
        let prefix = Self::edge_cluster_all_prefix().to_vec();
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
        let prefix = Self::edge_cluster_by_resource_prefix(resource);
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
        let by_id_key = Self::edge_cluster_by_id_key(edge_cluster_id);

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
                    tr.clear(&Self::edge_cluster_by_name_key(&record.name));
                    tr.clear(&Self::edge_cluster_all_key(record.id));
                    let now = Utc::now();
                    for resource in &record.bound_resources {
                        tr.clear(&Self::edge_cluster_by_resource_key(*resource, record.id));
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_ssh_key_public(
        &self,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        let scope = SshKeyScope::Public;
        let by_name_key = Self::ssh_key_by_public_name_key(&req.name);
        let by_fp_key = Self::ssh_key_by_public_fp_key(&fingerprint);
        let in_scope_key_for = |id: Uuid| Self::ssh_key_in_public_key(id);
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
        let scope = SshKeyScope::Silo { silo_id };
        let by_name_key = Self::ssh_key_by_silo_name_key(silo_id, &req.name);
        let by_fp_key = Self::ssh_key_by_silo_fp_key(silo_id, &fingerprint);
        let parent_check_key = Self::silo_by_id_key(silo_id);
        let in_scope_key_for = move |id: Uuid| Self::ssh_key_in_silo_key(silo_id, id);
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
        let scope = SshKeyScope::Tenant { tenant_id };
        let by_name_key = Self::ssh_key_by_tenant_name_key(tenant_id, &req.name);
        let by_fp_key = Self::ssh_key_by_tenant_fp_key(tenant_id, &fingerprint);
        let parent_check_key = Self::tenant_by_id_key(tenant_id);
        let in_scope_key_for = move |id: Uuid| Self::ssh_key_in_tenant_key(tenant_id, id);
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
        let scope = SshKeyScope::Project { project_id };
        let by_name_key = Self::ssh_key_by_project_name_key(project_id, &req.name);
        let by_fp_key = Self::ssh_key_by_project_fp_key(project_id, &fingerprint);
        let parent_check_key = Self::project_by_id_key(project_id);
        let in_scope_key_for = move |id: Uuid| Self::ssh_key_in_project_key(project_id, id);
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
        let scope = SshKeyScope::User { user_id };
        let by_name_key = Self::ssh_key_by_user_name_key(user_id, &req.name);
        let by_fp_key = Self::ssh_key_by_user_fp_key(user_id, &fingerprint);
        let parent_check_key = Self::user_by_id_key(user_id);
        let in_scope_key_for = move |id: Uuid| Self::ssh_key_by_user_idx_key(user_id, id);
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
        let key = Self::ssh_key_by_id_key(key_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize ssh key: {e}")))
    }

    async fn list_ssh_keys_public(&self) -> Result<Vec<SshKey>, StoreError> {
        let prefix = Self::ssh_key_in_public_prefix();
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_ssh_keys_in_silo(&self, silo_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = Self::ssh_key_in_silo_prefix(silo_id);
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_ssh_keys_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = Self::ssh_key_in_tenant_prefix(tenant_id);
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_ssh_keys_in_project(&self, project_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = Self::ssh_key_in_project_prefix(project_id);
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_ssh_keys_for_user(&self, user_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let prefix = Self::ssh_key_by_user_idx_prefix(user_id);
        self.list_ssh_keys_via_index(prefix).await
    }

    async fn list_visible_ssh_keys_in_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<SshKey>, StoreError> {
        let tenant_bytes = self
            .read_bytes(&Self::tenant_by_id_key(tenant_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let tenant: Tenant = serde_json::from_slice(&tenant_bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize tenant: {e}")))?;
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
            .read_bytes(&Self::project_by_id_key(project_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let project: Project = serde_json::from_slice(&project_bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize project: {e}")))?;
        let tenant_bytes = self
            .read_bytes(&Self::tenant_by_id_key(project.tenant_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let tenant: Tenant = serde_json::from_slice(&tenant_bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize tenant: {e}")))?;
        let mut out = self.list_ssh_keys_public().await?;
        out.extend(self.list_ssh_keys_in_silo(tenant.silo_id).await?);
        out.extend(self.list_ssh_keys_in_tenant(project.tenant_id).await?);
        out.extend(self.list_ssh_keys_in_project(project_id).await?);
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
        let (by_name_key, by_fp_key, in_scope_key) = match &key.scope {
            SshKeyScope::Public => (
                Self::ssh_key_by_public_name_key(&key.name),
                Self::ssh_key_by_public_fp_key(&key.fingerprint),
                Self::ssh_key_in_public_key(key.id),
            ),
            SshKeyScope::Silo { silo_id } => (
                Self::ssh_key_by_silo_name_key(*silo_id, &key.name),
                Self::ssh_key_by_silo_fp_key(*silo_id, &key.fingerprint),
                Self::ssh_key_in_silo_key(*silo_id, key.id),
            ),
            SshKeyScope::Tenant { tenant_id } => (
                Self::ssh_key_by_tenant_name_key(*tenant_id, &key.name),
                Self::ssh_key_by_tenant_fp_key(*tenant_id, &key.fingerprint),
                Self::ssh_key_in_tenant_key(*tenant_id, key.id),
            ),
            SshKeyScope::Project { project_id } => (
                Self::ssh_key_by_project_name_key(*project_id, &key.name),
                Self::ssh_key_by_project_fp_key(*project_id, &key.fingerprint),
                Self::ssh_key_in_project_key(*project_id, key.id),
            ),
            SshKeyScope::User { user_id } => (
                Self::ssh_key_by_user_name_key(*user_id, &key.name),
                Self::ssh_key_by_user_fp_key(*user_id, &key.fingerprint),
                Self::ssh_key_by_user_idx_key(*user_id, key.id),
            ),
        };

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
                let in_scope_key = in_scope_key.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DelOut::Vanished);
                    }
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_image_public(&self, req: NewImage) -> Result<Image, StoreError> {
        let scope = ImageScope::Public;
        let by_name_key = Self::image_by_public_name_key(&req.name);
        let in_scope_key_for = |id: Uuid| Self::image_in_public_key(id);
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
        let scope = ImageScope::Silo { silo_id };
        let by_name_key = Self::image_by_silo_name_key(silo_id, &req.name);
        let parent_check_key = Self::silo_by_id_key(silo_id);
        let in_scope_key_for = move |id: Uuid| Self::image_in_silo_key(silo_id, id);
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
        let scope = ImageScope::Tenant { tenant_id };
        let by_name_key = Self::image_by_tenant_name_key(tenant_id, &req.name);
        let parent_check_key = Self::tenant_by_id_key(tenant_id);
        let in_scope_key_for = move |id: Uuid| Self::image_in_tenant_key(tenant_id, id);
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
        let scope = ImageScope::Project { project_id };
        let by_name_key = Self::image_by_project_name_key(project_id, &req.name);
        let parent_check_key = Self::project_by_id_key(project_id);
        let in_scope_key_for = move |id: Uuid| Self::image_in_project_key(project_id, id);
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
        let scope = ImageScope::User { user_id };
        let by_name_key = Self::image_by_user_name_key(user_id, &req.name);
        let parent_check_key = Self::user_by_id_key(user_id);
        let in_scope_key_for = move |id: Uuid| Self::image_by_user_idx_key(user_id, id);
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
        let key = Self::image_by_id_key(image_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize image: {e}")))
    }

    async fn list_images_public(&self) -> Result<Vec<Image>, StoreError> {
        let prefix = Self::image_in_public_prefix();
        self.list_images_via_index(prefix).await
    }

    async fn list_images_in_silo(&self, silo_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = Self::image_in_silo_prefix(silo_id);
        self.list_images_via_index(prefix).await
    }

    async fn list_images_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = Self::image_in_tenant_prefix(tenant_id);
        self.list_images_via_index(prefix).await
    }

    async fn list_images_in_project(&self, project_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = Self::image_in_project_prefix(project_id);
        self.list_images_via_index(prefix).await
    }

    async fn list_images_for_user(&self, user_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let prefix = Self::image_by_user_idx_prefix(user_id);
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
            .read_bytes(&Self::tenant_by_id_key(tenant_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let tenant: Tenant = serde_json::from_slice(&tenant_bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize tenant: {e}")))?;
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
            .read_bytes(&Self::project_by_id_key(project_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let project: Project = serde_json::from_slice(&project_bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize project: {e}")))?;
        let tenant_bytes = self
            .read_bytes(&Self::tenant_by_id_key(project.tenant_id))
            .await?
            .ok_or(StoreError::NotFound)?;
        let tenant: Tenant = serde_json::from_slice(&tenant_bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize tenant: {e}")))?;
        let mut out = self.list_images_public().await?;
        out.extend(self.list_images_in_silo(tenant.silo_id).await?);
        out.extend(self.list_images_in_tenant(project.tenant_id).await?);
        out.extend(self.list_images_in_project(project_id).await?);
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
        let (by_name_key, in_scope_key) = match &image.scope {
            ImageScope::Public => (
                Self::image_by_public_name_key(&image.name),
                Self::image_in_public_key(image.id),
            ),
            ImageScope::Silo { silo_id } => (
                Self::image_by_silo_name_key(*silo_id, &image.name),
                Self::image_in_silo_key(*silo_id, image.id),
            ),
            ImageScope::Tenant { tenant_id } => (
                Self::image_by_tenant_name_key(*tenant_id, &image.name),
                Self::image_in_tenant_key(*tenant_id, image.id),
            ),
            ImageScope::Project { project_id } => (
                Self::image_by_project_name_key(*project_id, &image.name),
                Self::image_in_project_key(*project_id, image.id),
            ),
            ImageScope::User { user_id } => (
                Self::image_by_user_name_key(*user_id, &image.name),
                Self::image_by_user_idx_key(*user_id, image.id),
            ),
        };

        enum DelOut {
            Deleted,
            Vanished,
        }
        let outcome: Result<DelOut, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                let by_name_key = by_name_key.clone();
                let in_scope_key = in_scope_key.clone();
                async move {
                    if tr.get(&by_id_key, false).await?.is_none() {
                        return Ok(DelOut::Vanished);
                    }
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn put_quota(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewQuota,
    ) -> Result<Quota, StoreError> {
        let project_check_key = Self::project_by_id_key(project_id);
        let quota_key = Self::quota_by_project_key(project_id);
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
            .map_err(|e| StoreError::Backend(format!("serialize quota: {e}")))?;

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_quota(&self, tenant_id: Uuid, project_id: Uuid) -> Result<Quota, StoreError> {
        // Read project + quota inside a single transaction so the
        // tenant check is consistent with the read.
        let project_check_key = Self::project_by_id_key(project_id);
        let quota_key = Self::quota_by_project_key(project_id);

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn delete_quota(&self, tenant_id: Uuid, project_id: Uuid) -> Result<(), StoreError> {
        let project_check_key = Self::project_by_id_key(project_id);
        let quota_key = Self::quota_by_project_key(project_id);

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn create_instance(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewInstance,
    ) -> Result<InstanceCreateResult, StoreError> {
        // All cross-resource reads + the IP allocation set scan +
        // the instance write + the NIC write + the IP-alloc index
        // writes happen in a single transaction. A concurrent
        // delete of any referenced resource aborts cleanly; a
        // concurrent NIC create that would race for the same IP
        // is serialized by FDB's optimistic concurrency.
        let tenant_check_key = Self::tenant_by_id_key(tenant_id);
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

    async fn set_instance_host_cn(
        &self,
        instance_id: Uuid,
        host_cn_uuid: Option<Uuid>,
    ) -> Result<Instance, StoreError> {
        let by_id_key = Self::instance_by_id_key(instance_id);

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
                        tr.clear(&Self::instance_in_host_cn_key(old_host, instance_id));
                    }
                    if let Some(new_host) = host_cn_uuid {
                        tr.set(&Self::instance_in_host_cn_key(new_host, instance_id), b"");
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn list_instances_for_cn(&self, host_cn_uuid: Uuid) -> Result<Vec<Instance>, StoreError> {
        let prefix = Self::instance_in_host_cn_prefix(host_cn_uuid);
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
                .map_err(|e| StoreError::Backend(format!("instance host index uuid: {e}")))?;
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
                    if let Some(host_cn_uuid) = instance.host_cn_uuid {
                        tr.clear(&Self::instance_in_host_cn_key(host_cn_uuid, instance.id));
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
        tenant_id: Uuid,
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
                        Self::public_ip_holder_value(NetworkResourceId::FloatingIp { id: fip_id });
                    match address {
                        std::net::IpAddr::V4(v4) => {
                            tr.set(&Self::floating_ip_alloc_v4_key(v4), &holder);
                        }
                        std::net::IpAddr::V6(v6) => {
                            tr.set(&Self::floating_ip_alloc_v6_key(v6), &holder);
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
                                let value = serde_json::to_vec(&updated).map_err(|e| {
                                    FdbBindingError::CustomError(
                                        format!("serialize cn: {e}").into(),
                                    )
                                })?;
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
                                };
                                let value = serde_json::to_vec(&cn).map_err(|e| {
                                    FdbBindingError::CustomError(
                                        format!("serialize cn: {e}").into(),
                                    )
                                })?;
                                tr.set(&by_uuid_key, &value);
                                tr.set(&Self::cn_by_claim_key(&claim_code), &server_uuid_bytes);
                                tr.set(&Self::cn_by_poll_key(&poll_token), &server_uuid_bytes);
                                // Move the by_state membership to
                                // Pending (a no-op clear+set when the
                                // record was already Pending).
                                tr.clear(&Self::cn_by_state_key(prev_state, server_uuid));
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

    async fn set_cn_role(&self, server_uuid: Uuid, role: CnRole) -> Result<Cn, StoreError> {
        enum Outcome {
            Updated(Box<Cn>),
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
                    cn.role = role;
                    let value = serde_json::to_vec(&cn).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize cn: {e}").into())
                    })?;
                    tr.set(&by_uuid_key, &value);
                    Ok(Outcome::Updated(Box::new(cn)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Updated(cn)) => Ok(*cn),
            Ok(Outcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
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
                    cn.console_listen_port = console_listen_port;
                    cn.console_tls_spki_sha256 = console_tls_spki_sha256;
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

    async fn approve_cn(
        &self,
        server_uuid: Uuid,
        bound_api_key_id: Uuid,
        pending_credential: String,
        console_ticket_key: [u8; 32],
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
                    cn.console_ticket_key = Some(console_ticket_key);

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

    async fn upsert_legacy_vm(&self, legacy_vm: LegacyVm) -> Result<(), StoreError> {
        let smartos_uuid = legacy_vm.smartos_uuid;
        let new_host = legacy_vm.host_cn_uuid;
        let by_id_key = Self::legacy_vm_by_id_key(smartos_uuid);
        let instance_key = Self::instance_by_id_key(smartos_uuid);
        let new_membership_key = Self::legacy_vm_in_host_cn_key(new_host, smartos_uuid);
        let value = serde_json::to_vec(&legacy_vm)
            .map_err(|e| StoreError::Backend(format!("serialize legacy_vm: {e}")))?;

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
                            serde_json::from_slice(&existing_bytes).map_err(|e| {
                                FdbBindingError::CustomError(
                                    format!("deserialize legacy_vm: {e}").into(),
                                )
                            })?;
                        if existing.host_cn_uuid != new_host {
                            let old_membership_key =
                                Self::legacy_vm_in_host_cn_key(existing.host_cn_uuid, smartos_uuid);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_legacy_vm(&self, smartos_uuid: Uuid) -> Result<LegacyVm, StoreError> {
        let key = Self::legacy_vm_by_id_key(smartos_uuid);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize legacy_vm: {e}")))
    }

    async fn list_legacy_vms(&self) -> Result<Vec<LegacyVm>, StoreError> {
        let prefix = Self::legacy_vm_by_id_prefix().to_vec();
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
            bytes_list.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;
        let mut out = Vec::with_capacity(bytes_list.len());
        for bytes in bytes_list {
            let v: LegacyVm = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Backend(format!("deserialize legacy_vm: {e}")))?;
            out.push(v);
        }
        out.sort_by_key(|v| v.smartos_uuid);
        Ok(out)
    }

    async fn list_legacy_vms_for_cn(
        &self,
        host_cn_uuid: Uuid,
    ) -> Result<Vec<LegacyVm>, StoreError> {
        let prefix = Self::legacy_vm_in_host_cn_prefix(host_cn_uuid);
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
        let id_strs = id_strs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;
        let mut out = Vec::with_capacity(id_strs.len());
        for s in id_strs {
            let id = Uuid::parse_str(&s)
                .map_err(|e| StoreError::Backend(format!("legacy_vm host index uuid: {e}")))?;
            let by_id_key = Self::legacy_vm_by_id_key(id);
            if let Some(bytes) = self.read_bytes(&by_id_key).await? {
                let v: LegacyVm = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize legacy_vm: {e}")))?;
                out.push(v);
            }
        }
        out.sort_by_key(|v| v.smartos_uuid);
        Ok(out)
    }

    async fn delete_legacy_vm(&self, smartos_uuid: Uuid) -> Result<(), StoreError> {
        let by_id_key = Self::legacy_vm_by_id_key(smartos_uuid);
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id_key = by_id_key.clone();
                async move {
                    if let Some(bytes) = tr.get(&by_id_key, false).await? {
                        let existing: LegacyVm = serde_json::from_slice(&bytes).map_err(|e| {
                            FdbBindingError::CustomError(
                                format!("deserialize legacy_vm: {e}").into(),
                            )
                        })?;
                        let membership_key =
                            Self::legacy_vm_in_host_cn_key(existing.host_cn_uuid, smartos_uuid);
                        tr.clear(&membership_key);
                        tr.clear(&by_id_key);
                    }
                    // Idempotent: missing record is not an error.
                    Ok(())
                }
            })
            .await;
        result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))
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
        let key = Self::network_realization_key(resource, realizer);

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn list_network_realizations(
        &self,
        resource: NetworkResourceId,
    ) -> Result<Vec<Realization>, StoreError> {
        let prefix = Self::network_realization_resource_prefix(resource);
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

        let mut rows = result.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;
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
        let kvs = kvs.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(kvs.len());
        for bytes in kvs {
            let silo: Silo = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Backend(format!("deserialize silo: {e}")))?;
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
        let key = Self::dhcp_pool_by_vpc_key(vpc_id);
        match self.read_bytes(&key).await? {
            None => Ok(None),
            Some(bytes) => {
                let pool: DhcpPool = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Backend(format!("deserialize dhcp pool: {e}")))?;
                Ok(Some(pool))
            }
        }
    }

    async fn set_dhcp_pool(&self, vpc_id: Uuid, req: NewDhcpPool) -> Result<DhcpPool, StoreError> {
        let vpc_key = Self::vpc_by_id_key(vpc_id);
        let pool_key = Self::dhcp_pool_by_vpc_key(vpc_id);

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
                    let value = serde_json::to_vec(&pool).map_err(|e| {
                        FdbBindingError::CustomError(format!("serialize dhcp pool: {e}").into())
                    })?;
                    tr.set(&pool_key, &value);
                    Ok(Outcome::Stored(Box::new(pool)))
                }
            })
            .await;

        match outcome {
            Ok(Outcome::Stored(pool)) => Ok(*pool),
            Ok(Outcome::VpcMissing) => Err(StoreError::NotFound),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn clear_dhcp_pool(&self, vpc_id: Uuid) -> Result<(), StoreError> {
        let pool_key = Self::dhcp_pool_by_vpc_key(vpc_id);

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn list_dhcp_reservations(
        &self,
        vpc_id: Uuid,
    ) -> Result<Vec<DhcpReservation>, StoreError> {
        let prefix = Self::dhcp_reservation_by_vpc_prefix(vpc_id);
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
        let values = values.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(values.len());
        for bytes in values {
            let reservation: DhcpReservation = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Backend(format!("deserialize dhcp reservation: {e}")))?;
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
        let vpc_key = Self::vpc_by_id_key(vpc_id);
        let res_key = Self::dhcp_reservation_by_vpc_mac_key(vpc_id, &mac);

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
                    let value = serde_json::to_vec(&reservation).map_err(|e| {
                        FdbBindingError::CustomError(
                            format!("serialize dhcp reservation: {e}").into(),
                        )
                    })?;
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_dhcp_reservation(
        &self,
        vpc_id: Uuid,
        mac: &str,
    ) -> Result<DhcpReservation, StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let key = Self::dhcp_reservation_by_vpc_mac_key(vpc_id, &mac);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize dhcp reservation: {e}")))
    }

    async fn delete_dhcp_reservation(&self, vpc_id: Uuid, mac: &str) -> Result<(), StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let key = Self::dhcp_reservation_by_vpc_mac_key(vpc_id, &mac);

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn list_dhcp_leases(&self, vpc_id: Uuid) -> Result<Vec<DhcpLease>, StoreError> {
        let prefix = Self::dhcp_lease_by_vpc_prefix(vpc_id);
        self.scan_dhcp_leases(prefix).await.map(|mut v| {
            v.sort_by(|a, b| a.created_at.cmp(&b.created_at));
            v
        })
    }

    async fn get_dhcp_lease(&self, vpc_id: Uuid, mac: &str) -> Result<DhcpLease, StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let key = Self::dhcp_lease_by_vpc_mac_key(vpc_id, &mac);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize dhcp lease: {e}")))
    }

    async fn record_dhcp_lease(&self, mut lease: DhcpLease) -> Result<DhcpLease, StoreError> {
        lease.mac = crate::types::canonical_mac(&lease.mac)?;
        let key = Self::dhcp_lease_by_vpc_mac_key(lease.vpc_id, &lease.mac);
        let value = serde_json::to_vec(&lease)
            .map_err(|e| StoreError::Backend(format!("serialize dhcp lease: {e}")))?;

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
        Ok(lease)
    }

    async fn delete_dhcp_lease(&self, vpc_id: Uuid, mac: &str) -> Result<(), StoreError> {
        let mac = crate::types::canonical_mac(mac)?;
        let key = Self::dhcp_lease_by_vpc_mac_key(vpc_id, &mac);

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn list_all_dhcp_leases(&self) -> Result<Vec<DhcpLease>, StoreError> {
        let prefix = Self::dhcp_lease_global_prefix().to_vec();
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
        let id = Uuid::new_v4();
        let by_id_key = Self::storage_cluster_by_id_key(id);
        let by_name_key = Self::storage_cluster_by_name_key(&req.name);
        let all_key = Self::storage_cluster_all_key(id);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn get_storage_cluster(&self, id: Uuid) -> Result<StorageCluster, StoreError> {
        let key = Self::storage_cluster_by_id_key(id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize storage cluster: {e}")))
    }

    async fn get_storage_cluster_by_name(&self, name: &str) -> Result<StorageCluster, StoreError> {
        let by_name_key = Self::storage_cluster_by_name_key(name);
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
        let prefix = Self::storage_cluster_all_prefix().to_vec();
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
        let by_id_key = Self::storage_cluster_by_id_key(id);

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
                    tr.clear(&Self::storage_cluster_by_name_key(&cluster.name));
                    tr.clear(&Self::storage_cluster_all_key(cluster.id));
                    Ok(Out::Deleted)
                }
            })
            .await;

        match outcome {
            Ok(Out::Deleted) | Ok(Out::Vanished) => Ok(()),
            Ok(Out::Corrupt(e)) => Err(StoreError::Backend(format!(
                "deserialize storage cluster: {e}"
            ))),
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    async fn update_storage_cluster_status(
        &self,
        id: Uuid,
        status: StorageClusterStatus,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<StorageCluster, StoreError> {
        let by_id_key = Self::storage_cluster_by_id_key(id);

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
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
        let by_id_key = Self::storage_cluster_by_id_key(id);

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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
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

    async fn read_nat_gateway_record(
        &self,
        nat_gateway_id: Uuid,
    ) -> Result<NatGatewayRecord, StoreError> {
        let key = Self::nat_gateway_by_id_key(nat_gateway_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize nat gateway: {e}")))
    }

    /// Range-scan a `dhcp_lease/by_vpc/...` prefix and decode every
    /// value as a [`DhcpLease`]. Used by both the per-VPC list and
    /// the `list_all_dhcp_leases` reconciler-feeding scan.
    async fn scan_dhcp_leases(&self, prefix: Vec<u8>) -> Result<Vec<DhcpLease>, StoreError> {
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
        let values = values.map_err(|e| StoreError::Backend(format!("FDB transaction: {e}")))?;

        let mut out = Vec::with_capacity(values.len());
        for bytes in values {
            let lease: DhcpLease = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Backend(format!("deserialize dhcp lease: {e}")))?;
            out.push(lease);
        }
        Ok(out)
    }

    async fn read_edge_cluster_record(
        &self,
        edge_cluster_id: Uuid,
    ) -> Result<EdgeClusterRecord, StoreError> {
        let key = Self::edge_cluster_by_id_key(edge_cluster_id);
        let bytes = self.read_bytes(&key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Backend(format!("deserialize edge cluster: {e}")))
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
    async fn create_image_inner<F>(
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
            .map_err(|e| StoreError::Backend(format!("serialize image: {e}")))?;
        let by_id_key = Self::image_by_id_key(image.id);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    /// Shared body for the per-scope `list_images_*` methods.
    /// Walks a `image/in_*` membership-index prefix, parses the
    /// suffix uuids, then fetches each image record by id.
    async fn list_images_via_index(&self, prefix: Vec<u8>) -> Result<Vec<Image>, StoreError> {
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

    /// Shared body for the per-scope `create_ssh_key_*` methods.
    /// Mirrors [`Self::create_image_inner`]: optional parent
    /// existence check, then writes `ssh_key/by_id`, the
    /// per-scope name index, the per-scope fingerprint index,
    /// and the per-scope membership index. The id is
    /// content-addressed via [`crate::derive_ssh_key_id`] so
    /// idempotent re-create yields the same record.
    #[allow(clippy::too_many_arguments)] // 8 args is the natural shape for this helper.
    async fn create_ssh_key_inner<F>(
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
            .map_err(|e| StoreError::Backend(format!("serialize ssh key: {e}")))?;
        let by_id_key = Self::ssh_key_by_id_key(key.id);
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
            Err(e) => Err(StoreError::Backend(format!("FDB transaction: {e}"))),
        }
    }

    /// Shared body for the per-scope `list_ssh_keys_*` methods.
    /// Mirrors [`Self::list_images_via_index`].
    async fn list_ssh_keys_via_index(&self, prefix: Vec<u8>) -> Result<Vec<SshKey>, StoreError> {
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
    async fn settings_round_trip() {
        let store = fdb_test_store();
        // Reset to a known baseline first; another run may have left a
        // blob behind. (We never clear the keyspace wholesale.)
        store
            .put_settings(Settings::default())
            .await
            .expect("seed default settings");
        assert_eq!(
            store.get_settings().await.expect("get"),
            Settings::default()
        );

        let mut s = Settings::default();
        s.set(crate::ConfigKey::SweeperIntervalSecs, serde_json::json!(99))
            .unwrap();
        s.set(
            crate::ConfigKey::MetricsBackend,
            serde_json::json!("clickhouse"),
        )
        .unwrap();
        store.put_settings(s.clone()).await.expect("put");
        assert_eq!(store.get_settings().await.expect("get"), s);

        // Leave the singleton at defaults for the next run.
        store
            .put_settings(Settings::default())
            .await
            .expect("restore defaults");
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
        assert_eq!(cn.role, CnRole::Tenant);

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
            .approve_cn(id, Uuid::new_v4(), "tcadm_xxx".into(), [0u8; 32], now)
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
    async fn set_cn_role_updates_registered_cn() {
        let store = fdb_test_store();
        let id = Uuid::new_v4();
        purge_cn(&store, id).await;
        purge_window(&store).await;

        let now = Utc::now();
        store
            .register_cn(id, "edge-a".into(), None, sysinfo_fixture(), now)
            .await
            .expect("register");

        let updated = store.set_cn_role(id, CnRole::Edge).await.expect("set role");
        assert_eq!(updated.role, CnRole::Edge);
        assert_eq!(store.get_cn(id).await.expect("get").role, CnRole::Edge);

        let refreshed = store
            .register_cn(id, "edge-a-renamed".into(), None, sysinfo_fixture(), now)
            .await
            .expect("re-register");
        assert_eq!(refreshed.role, CnRole::Edge);

        let err = store
            .set_cn_role(Uuid::new_v4(), CnRole::Both)
            .await
            .expect_err("unknown cn");
        assert!(matches!(err, StoreError::NotFound));

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
            .approve_cn(id, key_id, "tcadm_secret".into(), [0u8; 32], now)
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
            .approve_cn(id, Uuid::new_v4(), "x".into(), [0u8; 32], Utc::now())
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
            .approve_cn(aid, Uuid::new_v4(), "k".into(), [0u8; 32], now)
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
mod route_target_tests {
    use super::*;

    fn nat_gateway_record(tenant_id: Uuid, project_id: Uuid, vpc_id: Uuid) -> NatGatewayRecord {
        let now = Utc::now();
        NatGatewayRecord {
            id: Uuid::new_v4(),
            tenant_id,
            project_id,
            vpc_id,
            name: "egress".to_string(),
            description: String::new(),
            family: AddressFamily::V4,
            public_address: "203.0.113.10".parse().unwrap(),
            edge_cluster_id: None,
            desired_generation: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn nat_gateway_route_target_must_belong_to_requested_vpc_scope() {
        let tenant_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let vpc_id = Uuid::new_v4();
        let nat = nat_gateway_record(tenant_id, project_id, vpc_id);

        FdbStore::validate_nat_gateway_route_target(&nat, tenant_id, project_id, vpc_id)
            .expect("matching NAT gateway scope should be accepted");

        let err = FdbStore::validate_nat_gateway_route_target(
            &nat,
            tenant_id,
            project_id,
            Uuid::new_v4(),
        )
        .expect_err("cross-VPC NAT gateway target should be rejected");
        assert!(matches!(err, StoreError::Conflict(_)));
    }
}

#[cfg(test)]
mod network_realization_tests {
    //! FDB-backed realization scan tests. Marked ignored because they
    //! require a running FoundationDB cluster. Run with
    //! `FDB_CLUSTER_FILE=/path/to/fdb.cluster cargo test -p tritond-store --features foundationdb empty_network_realization_scan_returns_empty_vec -- --ignored`.

    use super::*;

    #[tokio::test]
    #[ignore]
    async fn empty_network_realization_scan_returns_empty_vec() {
        let store = FdbStore::open(None).expect("open FDB cluster from default cluster file");
        let resource = NetworkResourceId::NatGateway { id: Uuid::new_v4() };

        let rows = store
            .list_network_realizations(resource)
            .await
            .expect("empty realization scan should succeed");

        assert!(rows.is_empty());
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
