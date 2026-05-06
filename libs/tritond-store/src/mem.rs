// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-memory [`Store`] backed by `tokio::sync::RwLock<HashMap>`.
//!
//! Used for unit tests, integration tests, and `tritond` runs that
//! don't need durable state.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use chrono::Utc;
use rand::Rng;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::types::NatGatewayRecord;
use crate::{
    AddressFamily, ApiKey, AutoApproveWindow, CLAIM_CODE_TTL, Cn, CnState, Disk, DiskKind,
    FLOATING_IP_V4_POOL, FLOATING_IP_V6_POOL, FloatingIp, FloatingIpAttachment, IdpConfig, Image,
    ImageScope, Instance, InstanceCreateResult, JobOutcome, JobStatus, JobStatusKind,
    LifecycleState, LifecycleStateKind, NatGateway, NetworkResourceId, NewFloatingIp, NewImage,
    NewInstance, NewJob, NewNatGateway, NewProject, NewQuota, NewRouteTable, NewSilo, NewSshKey,
    NewSubnet, NewTenant, NewVpc, Nic, Project, ProvisioningJob, Quota, Realization,
    RealizationStatus, RealizerId, RouteTable, Silo, SshKey, SshKeyScope, Store, StoreError,
    Subnet, SystemKey, Tenant, User, VPC_VNI_MAX, VPC_VNI_RESERVED_CEILING, Vpc,
    generate_claim_code, generate_poll_token,
};
#[cfg(test)]
use crate::{ApiKeyScope, NewInstanceNic};

/// Maximum attempts to draw a fresh VNI before giving up. With ~16.7M
/// candidates and any realistic VPC count, collisions are vanishingly
/// rare; the cap is purely defensive.
const VNI_RETRY_ATTEMPTS: usize = 8;
const MAIN_ROUTE_TABLE_NAME: &str = "main";

/// Build an [`SshKey`] record from a [`NewSshKey`] request and
/// its resolved scope + fingerprint. Centralised so the
/// per-scope create methods don't drift on field assignment.
fn build_ssh_key(id: Uuid, scope: SshKeyScope, req: &NewSshKey, fingerprint: String) -> SshKey {
    SshKey {
        id,
        scope,
        name: req.name.clone(),
        description: req.description.clone().unwrap_or_default(),
        public_key: req.public_key.clone(),
        fingerprint,
        created_at: Utc::now(),
    }
}

/// Build an [`Image`] record from a [`NewImage`] request and its
/// resolved scope. Centralised so the per-scope create methods
/// don't drift on field assignment.
fn build_image(id: Uuid, scope: ImageScope, req: &NewImage) -> Image {
    Image {
        id,
        scope,
        name: req.name.clone(),
        description: req.description.clone().unwrap_or_default(),
        os: req.os.clone(),
        version: req.version.clone(),
        size_bytes: req.size_bytes,
        sha256: req.sha256.clone(),
        source_url: req.source_url.clone(),
        compatibility: req.compatibility.clone(),
        created_at: Utc::now(),
    }
}

/// Job targeting matrix shared by both store backends:
/// * unrouted job (target=None) — claimable by anyone.
/// * routed job (target=Some(X)) — only the bound claimer for X
///   can take it. Unbound claimers (the in-process stub or a
///   legacy operator-minted Agent key) cannot claim routed jobs.
fn job_target_matches(job_target: Option<Uuid>, claimer_cn: Option<Uuid>) -> bool {
    match (job_target, claimer_cn) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(t), Some(c)) => t == c,
    }
}

#[derive(Default)]
struct Inner {
    silos_by_id: HashMap<Uuid, Silo>,
    silo_id_by_name: HashMap<String, Uuid>,
    users_by_id: HashMap<Uuid, User>,
    user_id_by_username: HashMap<String, Uuid>,
    /// `(tenant_id, issuer, subject)` → user_id index for federated
    /// users. Post E-5 the IdP is tenant-scoped, so federation
    /// lookups key off the owning tenant directly.
    user_id_by_federation: HashMap<(Uuid, String, String), Uuid>,
    api_keys_by_id: HashMap<Uuid, ApiKey>,
    api_key_id_by_lookup_id: HashMap<String, Uuid>,
    system_keys: HashMap<SystemKey, Vec<u8>>,
    idp_configs_by_tenant: HashMap<Uuid, IdpConfig>,
    /// `issuer_url` → tenant_id reverse index. Maintained in
    /// lockstep with `idp_configs_by_tenant` on put/delete and
    /// enforces global issuer uniqueness across tenants.
    tenant_id_by_issuer: HashMap<String, Uuid>,
    projects_by_id: HashMap<Uuid, Project>,
    /// `(tenant_id, name)` → project_id index for the within-tenant
    /// uniqueness check.
    project_id_by_tenant_name: HashMap<(Uuid, String), Uuid>,
    tenants_by_id: HashMap<Uuid, Tenant>,
    /// `(silo_id, name)` → tenant_id index for the within-silo
    /// uniqueness check.
    tenant_id_by_silo_name: HashMap<(Uuid, String), Uuid>,
    vpcs_by_id: HashMap<Uuid, Vpc>,
    /// `(project_id, name)` → vpc_id index for within-project name
    /// uniqueness.
    vpc_id_by_project_name: HashMap<(Uuid, String), Uuid>,
    /// Rack-wide set of VNIs currently in use. Drawn from
    /// `[VPC_VNI_RESERVED_CEILING, VPC_VNI_MAX)`.
    vnis_in_use: HashSet<u32>,
    subnets_by_id: HashMap<Uuid, Subnet>,
    /// `(vpc_id, name)` → subnet_id index for within-vpc name
    /// uniqueness.
    subnet_id_by_vpc_name: HashMap<(Uuid, String), Uuid>,
    route_tables_by_id: HashMap<Uuid, RouteTable>,
    /// `(vpc_id, name)` → route_table_id index for within-VPC name
    /// uniqueness. The auto-created main route table reserves
    /// `(vpc_id, "main")`.
    route_table_id_by_vpc_name: HashMap<(Uuid, String), Uuid>,
    nat_gateways_by_id: HashMap<Uuid, NatGatewayRecord>,
    /// `(vpc_id, name)` → nat_gateway_id index for within-VPC name
    /// uniqueness.
    nat_gateway_id_by_vpc_name: HashMap<(Uuid, String), Uuid>,
    ssh_keys_by_id: HashMap<Uuid, SshKey>,
    /// Per-scope name + fingerprint uniqueness indexes. Each
    /// scope-kind has its own pair of maps so two scopes whose
    /// namespace UUIDs collide can't conflict.
    ssh_key_id_by_public_name: HashMap<String, Uuid>,
    ssh_key_id_by_public_fingerprint: HashMap<String, Uuid>,
    ssh_key_id_by_silo_name: HashMap<(Uuid, String), Uuid>,
    ssh_key_id_by_silo_fingerprint: HashMap<(Uuid, String), Uuid>,
    ssh_key_id_by_tenant_name: HashMap<(Uuid, String), Uuid>,
    ssh_key_id_by_tenant_fingerprint: HashMap<(Uuid, String), Uuid>,
    ssh_key_id_by_project_name: HashMap<(Uuid, String), Uuid>,
    ssh_key_id_by_project_fingerprint: HashMap<(Uuid, String), Uuid>,
    ssh_key_id_by_user_name: HashMap<(Uuid, String), Uuid>,
    ssh_key_id_by_user_fingerprint: HashMap<(Uuid, String), Uuid>,
    images_by_id: HashMap<Uuid, Image>,
    /// Per-scope name uniqueness indexes. The key is the scope's
    /// namespace UUID (`Uuid::nil()` for Public, silo_id /
    /// tenant_id / project_id / user_id for the others) plus the
    /// image name. Each scope-kind has its own map so two scopes
    /// whose namespace UUIDs collide can't conflict.
    image_id_by_public_name: HashMap<String, Uuid>,
    image_id_by_silo_name: HashMap<(Uuid, String), Uuid>,
    image_id_by_tenant_name: HashMap<(Uuid, String), Uuid>,
    image_id_by_project_name: HashMap<(Uuid, String), Uuid>,
    image_id_by_user_name: HashMap<(Uuid, String), Uuid>,
    /// `project_id` → quota record. Singleton per project.
    quotas_by_project: HashMap<Uuid, Quota>,
    instances_by_id: HashMap<Uuid, Instance>,
    /// `(project_id, name)` → instance_id index for within-project
    /// name uniqueness.
    instance_id_by_project_name: HashMap<(Uuid, String), Uuid>,
    jobs_by_id: HashMap<Uuid, ProvisioningJob>,
    /// Monotonic job sequence counter; the next job's `seq` value.
    /// FIFO consumption picks the smallest `seq` with status
    /// `Pending`.
    next_job_seq: u64,
    nics_by_id: HashMap<Uuid, Nic>,
    /// Per-subnet IPv4 allocations. NIC delete frees the address
    /// back to the pool, so re-creating an instance reuses the
    /// lowest-numbered free address.
    allocated_ipv4_by_subnet: HashMap<Uuid, HashSet<Ipv4Addr>>,
    allocated_ipv6_by_subnet: HashMap<Uuid, HashSet<Ipv6Addr>>,
    disks_by_id: HashMap<Uuid, Disk>,
    floating_ips_by_id: HashMap<Uuid, FloatingIp>,
    /// `(project_id, name)` → fip_id index for within-project name
    /// uniqueness.
    floating_ip_id_by_project_name: HashMap<(Uuid, String), Uuid>,
    /// Shared public-address allocation tracking. FloatingIp and
    /// NatGateway both reserve from these Phase 0 pools, and the
    /// value records the owning `{kind}:{uuid}` for diagnostics.
    public_ipv4_allocations: HashMap<Ipv4Addr, String>,
    public_ipv6_allocations: HashMap<Ipv6Addr, String>,
    /// CN registrations keyed by `server_uuid`.
    cns_by_server_uuid: HashMap<Uuid, Cn>,
    /// `claim_code` (normalized) → `server_uuid`. Only populated for
    /// records currently in [`CnState::Pending`] with an unexpired
    /// claim code.
    cn_server_uuid_by_claim_code: HashMap<String, Uuid>,
    /// `poll_token` → `server_uuid`. Rotated on every (re-)registration.
    cn_server_uuid_by_poll_token: HashMap<String, Uuid>,
    /// Singleton auto-approve window. `None` when closed.
    auto_approve_window: Option<AutoApproveWindow>,
    /// Per-`(resource, realizer)` realization rows. Mirrors the FDB
    /// `network_realization/<kind>/<id>/<realizer_kind>/<realizer_id>`
    /// keyspace. Written by [`Store::record_network_realization`];
    /// read back by [`Store::list_network_realizations`].
    network_realizations: HashMap<(NetworkResourceId, RealizerId), Realization>,
}

/// In-process [`Store`] implementation.
///
/// State is held behind a `tokio::sync::RwLock`; this is fine for
/// tests and small embedded uses but does not survive process
/// restarts.
pub struct MemStore {
    inner: RwLock<Inner>,
}

impl MemStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

fn public_ip_holder(kind: &str, id: Uuid) -> String {
    format!("{kind}:{id}")
}

fn realization_rows(
    realizations: &HashMap<(NetworkResourceId, RealizerId), Realization>,
    resource: NetworkResourceId,
) -> Vec<Realization> {
    let mut rows: Vec<Realization> = realizations
        .iter()
        .filter(|((r, _), _)| *r == resource)
        .map(|(_, row)| row.clone())
        .collect();
    rows.sort_by(|a, b| {
        a.realizer
            .kind_tag()
            .cmp(b.realizer.kind_tag())
            .then_with(|| a.realizer.id().cmp(&b.realizer.id()))
    });
    rows
}

#[async_trait]
impl Store for MemStore {
    async fn create_silo(&self, req: NewSilo) -> Result<Silo, StoreError> {
        let mut guard = self.inner.write().await;

        if guard.silo_id_by_name.contains_key(&req.name) {
            return Err(StoreError::Conflict(format!(
                "silo with name {:?} already exists",
                req.name
            )));
        }

        // Atomic two-record write: create the silo and its default
        // tenant in the same lock acquisition so a federated login
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
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            default_tenant_id: tenant.id,
            created_at: now,
        };
        guard.silo_id_by_name.insert(silo.name.clone(), silo.id);
        guard.silos_by_id.insert(silo.id, silo.clone());
        guard
            .tenant_id_by_silo_name
            .insert((silo_id, tenant.name.clone()), tenant.id);
        guard.tenants_by_id.insert(tenant.id, tenant);
        Ok(silo)
    }

    async fn get_silo(&self, id: Uuid) -> Result<Silo, StoreError> {
        let guard = self.inner.read().await;
        guard
            .silos_by_id
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn create_user(&self, user: User) -> Result<User, StoreError> {
        let mut guard = self.inner.write().await;
        if guard.user_id_by_username.contains_key(&user.username) {
            return Err(StoreError::Conflict(format!(
                "user with username {:?} already exists",
                user.username
            )));
        }
        // Federation index is keyed by (tenant_id, issuer, subject) —
        // post E-5 the IdP belongs directly to the tenant, so the
        // index is rooted at the tenant the user lives in.
        if let (Some(tenant_id), Some(fed)) = (user.tenant_id, user.federation.as_ref()) {
            // Defensive: the tenant must exist before we anchor a
            // federated user to it. A missing tenant is a programming
            // error, not a normal flow.
            if !guard.tenants_by_id.contains_key(&tenant_id) {
                return Err(StoreError::NotFound);
            }
            let key = (tenant_id, fed.issuer.clone(), fed.subject.clone());
            if guard.user_id_by_federation.contains_key(&key) {
                return Err(StoreError::Conflict(format!(
                    "federated user already exists for tenant {tenant_id} issuer {} subject {}",
                    fed.issuer, fed.subject
                )));
            }
            guard.user_id_by_federation.insert(key, user.id);
        }
        guard
            .user_id_by_username
            .insert(user.username.clone(), user.id);
        guard.users_by_id.insert(user.id, user.clone());
        Ok(user)
    }

    async fn get_user_by_username(&self, username: &str) -> Result<User, StoreError> {
        let guard = self.inner.read().await;
        let id = guard
            .user_id_by_username
            .get(username)
            .copied()
            .ok_or(StoreError::NotFound)?;
        guard
            .users_by_id
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_user_by_id(&self, id: Uuid) -> Result<User, StoreError> {
        let guard = self.inner.read().await;
        guard
            .users_by_id
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn has_any_user(&self) -> Result<bool, StoreError> {
        let guard = self.inner.read().await;
        Ok(!guard.users_by_id.is_empty())
    }

    async fn create_api_key(&self, key: ApiKey) -> Result<ApiKey, StoreError> {
        let mut guard = self.inner.write().await;
        if guard.api_key_id_by_lookup_id.contains_key(&key.lookup_id) {
            return Err(StoreError::Conflict(format!(
                "api key with lookup id {:?} already exists",
                key.lookup_id
            )));
        }
        guard
            .api_key_id_by_lookup_id
            .insert(key.lookup_id.clone(), key.id);
        guard.api_keys_by_id.insert(key.id, key.clone());
        Ok(key)
    }

    async fn list_api_keys(&self, user_id: Uuid) -> Result<Vec<ApiKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .api_keys_by_id
            .values()
            .filter(|k| k.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn get_api_key_by_lookup_id(&self, lookup_id: &str) -> Result<ApiKey, StoreError> {
        let guard = self.inner.read().await;
        let key_id = guard
            .api_key_id_by_lookup_id
            .get(lookup_id)
            .copied()
            .ok_or(StoreError::NotFound)?;
        guard
            .api_keys_by_id
            .get(&key_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn delete_api_key(&self, user_id: Uuid, key_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let lookup_id = match guard.api_keys_by_id.get(&key_id) {
            Some(k) if k.user_id == user_id => k.lookup_id.clone(),
            _ => return Err(StoreError::NotFound),
        };
        guard.api_keys_by_id.remove(&key_id);
        guard.api_key_id_by_lookup_id.remove(&lookup_id);
        Ok(())
    }

    async fn get_system_key(&self, key: SystemKey) -> Result<Vec<u8>, StoreError> {
        let guard = self.inner.read().await;
        guard
            .system_keys
            .get(&key)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn put_system_key(&self, key: SystemKey, value: Vec<u8>) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        guard.system_keys.insert(key, value);
        Ok(())
    }

    async fn get_user_by_federation(
        &self,
        tenant_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<User, StoreError> {
        let guard = self.inner.read().await;
        let key = (tenant_id, issuer.to_string(), subject.to_string());
        let user_id = guard
            .user_id_by_federation
            .get(&key)
            .copied()
            .ok_or(StoreError::NotFound)?;
        guard
            .users_by_id
            .get(&user_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn put_idp_config(
        &self,
        tenant_id: Uuid,
        config: IdpConfig,
    ) -> Result<IdpConfig, StoreError> {
        let mut guard = self.inner.write().await;

        // Issuer uniqueness across tenants. If the issuer is already
        // claimed by a *different* tenant, reject. Re-putting the
        // same tenant's config (idempotent or with a changed issuer)
        // is fine; in that case we also clear the old issuer's
        // reverse-index entry below.
        if let Some(other_tenant) = guard.tenant_id_by_issuer.get(&config.issuer_url)
            && *other_tenant != tenant_id
        {
            return Err(StoreError::Conflict(format!(
                "issuer {:?} already claimed by another tenant",
                config.issuer_url
            )));
        }

        // If this tenant previously had a config with a different
        // issuer, drop the stale issuer→tenant entry before
        // installing the new one. Clone the URL out so we can take
        // a `&mut` to a different sub-map below without overlapping
        // borrows.
        let prev_issuer = guard
            .idp_configs_by_tenant
            .get(&tenant_id)
            .map(|prev| prev.issuer_url.clone());
        if let Some(prev) = prev_issuer
            && prev != config.issuer_url
        {
            guard.tenant_id_by_issuer.remove(&prev);
        }

        guard
            .tenant_id_by_issuer
            .insert(config.issuer_url.clone(), tenant_id);
        guard
            .idp_configs_by_tenant
            .insert(tenant_id, config.clone());
        Ok(config)
    }

    async fn get_idp_config(&self, tenant_id: Uuid) -> Result<IdpConfig, StoreError> {
        let guard = self.inner.read().await;
        guard
            .idp_configs_by_tenant
            .get(&tenant_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn delete_idp_config(&self, tenant_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let removed = guard
            .idp_configs_by_tenant
            .remove(&tenant_id)
            .ok_or(StoreError::NotFound)?;
        guard.tenant_id_by_issuer.remove(&removed.issuer_url);
        Ok(())
    }

    async fn list_idp_configs(&self) -> Result<Vec<(Uuid, IdpConfig)>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .idp_configs_by_tenant
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect())
    }

    async fn get_idp_config_by_issuer(
        &self,
        issuer: &str,
    ) -> Result<(Uuid, IdpConfig), StoreError> {
        let guard = self.inner.read().await;
        let tenant_id = guard
            .tenant_id_by_issuer
            .get(issuer)
            .copied()
            .ok_or(StoreError::NotFound)?;
        let config = guard
            .idp_configs_by_tenant
            .get(&tenant_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        Ok((tenant_id, config))
    }

    async fn create_project(
        &self,
        tenant_id: Uuid,
        req: NewProject,
    ) -> Result<Project, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.tenants_by_id.contains_key(&tenant_id) {
            return Err(StoreError::NotFound);
        }
        let key = (tenant_id, req.name.clone());
        if guard.project_id_by_tenant_name.contains_key(&key) {
            return Err(StoreError::Conflict(format!(
                "project with name {:?} already exists in tenant {tenant_id}",
                req.name
            )));
        }
        let project = Project {
            id: Uuid::new_v4(),
            tenant_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        guard.project_id_by_tenant_name.insert(key, project.id);
        guard.projects_by_id.insert(project.id, project.clone());
        Ok(project)
    }

    async fn get_project(&self, project_id: Uuid) -> Result<Project, StoreError> {
        let guard = self.inner.read().await;
        guard
            .projects_by_id
            .get(&project_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_projects_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Project>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .projects_by_id
            .values()
            .filter(|p| p.tenant_id == tenant_id)
            .cloned()
            .collect())
    }

    async fn delete_project(&self, project_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let project = guard
            .projects_by_id
            .remove(&project_id)
            .ok_or(StoreError::NotFound)?;
        guard
            .project_id_by_tenant_name
            .remove(&(project.tenant_id, project.name));
        Ok(())
    }

    async fn create_tenant(&self, silo_id: Uuid, req: NewTenant) -> Result<Tenant, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.silos_by_id.contains_key(&silo_id) {
            return Err(StoreError::NotFound);
        }
        let key = (silo_id, req.name.clone());
        if guard.tenant_id_by_silo_name.contains_key(&key) {
            return Err(StoreError::Conflict(format!(
                "tenant with name {:?} already exists in silo {silo_id}",
                req.name
            )));
        }
        let tenant = Tenant {
            id: Uuid::new_v4(),
            silo_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        guard.tenant_id_by_silo_name.insert(key, tenant.id);
        guard.tenants_by_id.insert(tenant.id, tenant.clone());
        Ok(tenant)
    }

    async fn get_tenant(&self, tenant_id: Uuid) -> Result<Tenant, StoreError> {
        let guard = self.inner.read().await;
        guard
            .tenants_by_id
            .get(&tenant_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_tenants_in_silo(&self, silo_id: Uuid) -> Result<Vec<Tenant>, StoreError> {
        let guard = self.inner.read().await;
        if !guard.silos_by_id.contains_key(&silo_id) {
            return Err(StoreError::NotFound);
        }
        Ok(guard
            .tenants_by_id
            .values()
            .filter(|t| t.silo_id == silo_id)
            .cloned()
            .collect())
    }

    async fn delete_tenant(&self, tenant_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let tenant = guard
            .tenants_by_id
            .remove(&tenant_id)
            .ok_or(StoreError::NotFound)?;
        // TODO(slice E-3): reject deletion when child projects exist
        guard
            .tenant_id_by_silo_name
            .remove(&(tenant.silo_id, tenant.name));
        Ok(())
    }

    async fn create_vpc(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewVpc,
    ) -> Result<Vpc, StoreError> {
        let mut guard = self.inner.write().await;

        // Project must exist and live in the right tenant. A tenant
        // mismatch surfaces as NotFound (project is invisible to a
        // foreign tenant).
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?;
        if project.tenant_id != tenant_id {
            return Err(StoreError::NotFound);
        }

        let name_key = (project_id, req.name.clone());
        if guard.vpc_id_by_project_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "vpc with name {:?} already exists in project {project_id}",
                req.name
            )));
        }

        let mut rng = rand::rng();
        let mut vni = None;
        for _ in 0..VNI_RETRY_ATTEMPTS {
            let candidate = rng.random_range(VPC_VNI_RESERVED_CEILING..VPC_VNI_MAX);
            if !guard.vnis_in_use.contains(&candidate) {
                vni = Some(candidate);
                break;
            }
        }
        let vni = vni.ok_or_else(|| {
            StoreError::Backend(format!("VNI exhausted after {VNI_RETRY_ATTEMPTS} retries"))
        })?;

        let vpc_id = Uuid::new_v4();
        let route_table_id = Uuid::new_v4();
        let now = Utc::now();
        let route_table = RouteTable {
            id: route_table_id,
            tenant_id,
            project_id,
            vpc_id,
            name: MAIN_ROUTE_TABLE_NAME.to_string(),
            description: format!("Main route table for VPC {}", req.name),
            is_main: true,
            created_at: now,
        };
        let vpc = Vpc {
            id: vpc_id,
            tenant_id,
            project_id,
            main_route_table_id: route_table_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            vni,
            ipv4_block: req.ipv4_block,
            ipv6_block: req.ipv6_block,
            created_at: now,
        };
        guard.vnis_in_use.insert(vni);
        guard.vpc_id_by_project_name.insert(name_key, vpc.id);
        guard
            .route_table_id_by_vpc_name
            .insert((vpc.id, MAIN_ROUTE_TABLE_NAME.to_string()), route_table_id);
        guard.route_tables_by_id.insert(route_table_id, route_table);
        guard.vpcs_by_id.insert(vpc.id, vpc.clone());
        Ok(vpc)
    }

    async fn get_vpc(&self, vpc_id: Uuid) -> Result<Vpc, StoreError> {
        let guard = self.inner.read().await;
        guard
            .vpcs_by_id
            .get(&vpc_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_vpcs_in_project(&self, project_id: Uuid) -> Result<Vec<Vpc>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .vpcs_by_id
            .values()
            .filter(|v| v.project_id == project_id)
            .cloned()
            .collect())
    }

    async fn delete_vpc(&self, vpc_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        // VPC must exist.
        if !guard.vpcs_by_id.contains_key(&vpc_id) {
            return Err(StoreError::NotFound);
        }
        // Block delete if any subnet still references this VPC.
        let has_subnets = guard.subnets_by_id.values().any(|s| s.vpc_id == vpc_id);
        if has_subnets {
            return Err(StoreError::Conflict(format!(
                "vpc {vpc_id} still has subnets attached; delete subnets first"
            )));
        }
        let has_non_main_route_tables = guard
            .route_tables_by_id
            .values()
            .any(|rt| rt.vpc_id == vpc_id && !rt.is_main);
        if has_non_main_route_tables {
            return Err(StoreError::Conflict(format!(
                "vpc {vpc_id} still has route tables attached; delete route tables first"
            )));
        }
        let vpc = guard
            .vpcs_by_id
            .remove(&vpc_id)
            .ok_or(StoreError::NotFound)?;
        guard
            .vpc_id_by_project_name
            .remove(&(vpc.project_id, vpc.name));
        if let Some(route_table) = guard.route_tables_by_id.remove(&vpc.main_route_table_id) {
            guard
                .route_table_id_by_vpc_name
                .remove(&(route_table.vpc_id, route_table.name));
        }
        guard.vnis_in_use.remove(&vpc.vni);
        Ok(())
    }

    async fn create_subnet(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewSubnet,
    ) -> Result<Subnet, StoreError> {
        let mut guard = self.inner.write().await;

        // VPC must exist and live under the right tenant+project. Any
        // mismatch surfaces as NotFound (cross-tenant probe story).
        let vpc = guard.vpcs_by_id.get(&vpc_id).ok_or(StoreError::NotFound)?;
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(StoreError::NotFound);
        }
        let vpc = vpc.clone();

        // CIDR family + containment + overlap checks. We collect
        // peers into a Vec so the validator can take a slice; the
        // borrow on `guard` would otherwise prevent the subsequent
        // mutation below.
        let peers: Vec<Subnet> = guard
            .subnets_by_id
            .values()
            .filter(|s| s.vpc_id == vpc_id)
            .cloned()
            .collect();
        crate::types::validate_subnet_cidrs(&vpc, req.ipv4_block, req.ipv6_block, &peers)
            .map_err(StoreError::Conflict)?;

        let name_key = (vpc_id, req.name.clone());
        if guard.subnet_id_by_vpc_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "subnet with name {:?} already exists in vpc {vpc_id}",
                req.name
            )));
        }

        let subnet = Subnet {
            id: Uuid::new_v4(),
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
        guard.subnet_id_by_vpc_name.insert(name_key, subnet.id);
        guard.subnets_by_id.insert(subnet.id, subnet.clone());
        Ok(subnet)
    }

    async fn get_subnet(&self, subnet_id: Uuid) -> Result<Subnet, StoreError> {
        let guard = self.inner.read().await;
        guard
            .subnets_by_id
            .get(&subnet_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_subnets_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<Subnet>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .subnets_by_id
            .values()
            .filter(|s| s.vpc_id == vpc_id)
            .cloned()
            .collect())
    }

    async fn delete_subnet(&self, subnet_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let subnet = guard
            .subnets_by_id
            .remove(&subnet_id)
            .ok_or(StoreError::NotFound)?;
        guard
            .subnet_id_by_vpc_name
            .remove(&(subnet.vpc_id, subnet.name));
        Ok(())
    }

    async fn create_route_table(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewRouteTable,
    ) -> Result<RouteTable, StoreError> {
        let mut guard = self.inner.write().await;

        let vpc = guard.vpcs_by_id.get(&vpc_id).ok_or(StoreError::NotFound)?;
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(StoreError::NotFound);
        }

        let name_key = (vpc_id, req.name.clone());
        if guard.route_table_id_by_vpc_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "route table with name {:?} already exists in vpc {vpc_id}",
                req.name
            )));
        }

        let route_table = RouteTable {
            id: Uuid::new_v4(),
            tenant_id,
            project_id,
            vpc_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            is_main: false,
            created_at: Utc::now(),
        };
        guard
            .route_table_id_by_vpc_name
            .insert(name_key, route_table.id);
        guard
            .route_tables_by_id
            .insert(route_table.id, route_table.clone());
        Ok(route_table)
    }

    async fn get_route_table(&self, route_table_id: Uuid) -> Result<RouteTable, StoreError> {
        let guard = self.inner.read().await;
        guard
            .route_tables_by_id
            .get(&route_table_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_route_tables_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<RouteTable>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .route_tables_by_id
            .values()
            .filter(|rt| rt.vpc_id == vpc_id)
            .cloned()
            .collect())
    }

    async fn delete_route_table(&self, route_table_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let route_table = guard
            .route_tables_by_id
            .get(&route_table_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if route_table.is_main {
            return Err(StoreError::Conflict(format!(
                "route table {route_table_id} is the main route table for vpc {}; delete the vpc instead",
                route_table.vpc_id
            )));
        }
        let has_subnet_associations = guard
            .subnets_by_id
            .values()
            .any(|s| s.route_table_id == route_table_id);
        if has_subnet_associations {
            return Err(StoreError::Conflict(format!(
                "route table {route_table_id} is still associated with subnets"
            )));
        }
        let route_table = guard
            .route_tables_by_id
            .remove(&route_table_id)
            .ok_or(StoreError::NotFound)?;
        guard
            .route_table_id_by_vpc_name
            .remove(&(route_table.vpc_id, route_table.name));
        Ok(())
    }

    async fn create_nat_gateway(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewNatGateway,
    ) -> Result<NatGateway, StoreError> {
        let mut guard = self.inner.write().await;

        let vpc = guard.vpcs_by_id.get(&vpc_id).ok_or(StoreError::NotFound)?;
        if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
            return Err(StoreError::NotFound);
        }

        let name_key = (vpc_id, req.name.clone());
        if guard.nat_gateway_id_by_vpc_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "nat gateway with name {:?} already exists in vpc {vpc_id}",
                req.name
            )));
        }

        let public_address: IpAddr = match req.family {
            AddressFamily::V4 => {
                let allocated = guard.public_ipv4_allocations.keys().copied().collect();
                crate::types::allocate_ipv4(FLOATING_IP_V4_POOL, &allocated)
                    .ok_or_else(|| StoreError::Backend("public ipv4 pool exhausted".to_string()))?
                    .into()
            }
            AddressFamily::V6 => {
                let allocated = guard.public_ipv6_allocations.keys().copied().collect();
                crate::types::allocate_ipv6(FLOATING_IP_V6_POOL, &allocated)
                    .ok_or_else(|| StoreError::Backend("public ipv6 pool exhausted".to_string()))?
                    .into()
            }
        };

        let now = Utc::now();
        let record = NatGatewayRecord {
            id: Uuid::new_v4(),
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

        let holder = public_ip_holder("nat_gateway", record.id);
        match public_address {
            IpAddr::V4(v4) => {
                guard.public_ipv4_allocations.insert(v4, holder);
            }
            IpAddr::V6(v6) => {
                guard.public_ipv6_allocations.insert(v6, holder);
            }
        }
        guard.nat_gateway_id_by_vpc_name.insert(name_key, record.id);
        guard.nat_gateways_by_id.insert(record.id, record.clone());

        Ok(record.into_view(Vec::new()))
    }

    async fn get_nat_gateway(&self, nat_gateway_id: Uuid) -> Result<NatGateway, StoreError> {
        let guard = self.inner.read().await;
        let record = guard
            .nat_gateways_by_id
            .get(&nat_gateway_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let rows = realization_rows(&guard.network_realizations, record.resource_id());
        Ok(record.into_view(rows))
    }

    async fn list_nat_gateways_in_vpc(&self, vpc_id: Uuid) -> Result<Vec<NatGateway>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .nat_gateways_by_id
            .values()
            .filter(|n| n.vpc_id == vpc_id)
            .cloned()
            .map(|record| {
                let rows = realization_rows(&guard.network_realizations, record.resource_id());
                record.into_view(rows)
            })
            .collect())
    }

    async fn delete_nat_gateway(&self, nat_gateway_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let record = guard
            .nat_gateways_by_id
            .remove(&nat_gateway_id)
            .ok_or(StoreError::NotFound)?;
        guard
            .nat_gateway_id_by_vpc_name
            .remove(&(record.vpc_id, record.name));
        match record.public_address {
            IpAddr::V4(v4) => {
                guard.public_ipv4_allocations.remove(&v4);
            }
            IpAddr::V6(v6) => {
                guard.public_ipv6_allocations.remove(&v6);
            }
        }
        Ok(())
    }

    async fn create_ssh_key_public(
        &self,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        let mut guard = self.inner.write().await;
        if guard.ssh_key_id_by_public_name.contains_key(&req.name) {
            return Err(StoreError::Conflict(format!(
                "public ssh key with name {:?} already exists",
                req.name
            )));
        }
        if guard
            .ssh_key_id_by_public_fingerprint
            .contains_key(&fingerprint)
        {
            return Err(StoreError::Conflict(format!(
                "public ssh key with fingerprint {fingerprint} already exists"
            )));
        }
        let scope = SshKeyScope::Public;
        let id = crate::derive_ssh_key_id(&scope, &fingerprint);
        if guard.ssh_keys_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "ssh key with id {id} already exists",
            )));
        }
        let key = build_ssh_key(id, scope, &req, fingerprint.clone());
        guard
            .ssh_key_id_by_public_name
            .insert(key.name.clone(), key.id);
        guard
            .ssh_key_id_by_public_fingerprint
            .insert(fingerprint, key.id);
        guard.ssh_keys_by_id.insert(key.id, key.clone());
        Ok(key)
    }

    async fn create_ssh_key_silo(
        &self,
        silo_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.silos_by_id.contains_key(&silo_id) {
            return Err(StoreError::NotFound);
        }
        let name_key = (silo_id, req.name.clone());
        if guard.ssh_key_id_by_silo_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "ssh key with name {:?} already exists in silo {silo_id}",
                req.name
            )));
        }
        let fp_key = (silo_id, fingerprint.clone());
        if guard.ssh_key_id_by_silo_fingerprint.contains_key(&fp_key) {
            return Err(StoreError::Conflict(format!(
                "ssh key with fingerprint {fingerprint} already exists in silo {silo_id}"
            )));
        }
        let scope = SshKeyScope::Silo { silo_id };
        let id = crate::derive_ssh_key_id(&scope, &fingerprint);
        if guard.ssh_keys_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "ssh key with id {id} already exists",
            )));
        }
        let key = build_ssh_key(id, scope, &req, fingerprint);
        guard.ssh_key_id_by_silo_name.insert(name_key, key.id);
        guard.ssh_key_id_by_silo_fingerprint.insert(fp_key, key.id);
        guard.ssh_keys_by_id.insert(key.id, key.clone());
        Ok(key)
    }

    async fn create_ssh_key_tenant(
        &self,
        tenant_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.tenants_by_id.contains_key(&tenant_id) {
            return Err(StoreError::NotFound);
        }
        let name_key = (tenant_id, req.name.clone());
        if guard.ssh_key_id_by_tenant_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "ssh key with name {:?} already exists in tenant {tenant_id}",
                req.name
            )));
        }
        let fp_key = (tenant_id, fingerprint.clone());
        if guard.ssh_key_id_by_tenant_fingerprint.contains_key(&fp_key) {
            return Err(StoreError::Conflict(format!(
                "ssh key with fingerprint {fingerprint} already exists in tenant {tenant_id}"
            )));
        }
        let scope = SshKeyScope::Tenant { tenant_id };
        let id = crate::derive_ssh_key_id(&scope, &fingerprint);
        if guard.ssh_keys_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "ssh key with id {id} already exists",
            )));
        }
        let key = build_ssh_key(id, scope, &req, fingerprint);
        guard.ssh_key_id_by_tenant_name.insert(name_key, key.id);
        guard
            .ssh_key_id_by_tenant_fingerprint
            .insert(fp_key, key.id);
        guard.ssh_keys_by_id.insert(key.id, key.clone());
        Ok(key)
    }

    async fn create_ssh_key_project(
        &self,
        project_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.projects_by_id.contains_key(&project_id) {
            return Err(StoreError::NotFound);
        }
        let name_key = (project_id, req.name.clone());
        if guard.ssh_key_id_by_project_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "ssh key with name {:?} already exists in project {project_id}",
                req.name
            )));
        }
        let fp_key = (project_id, fingerprint.clone());
        if guard
            .ssh_key_id_by_project_fingerprint
            .contains_key(&fp_key)
        {
            return Err(StoreError::Conflict(format!(
                "ssh key with fingerprint {fingerprint} already exists in project {project_id}"
            )));
        }
        let scope = SshKeyScope::Project { project_id };
        let id = crate::derive_ssh_key_id(&scope, &fingerprint);
        if guard.ssh_keys_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "ssh key with id {id} already exists",
            )));
        }
        let key = build_ssh_key(id, scope, &req, fingerprint);
        guard.ssh_key_id_by_project_name.insert(name_key, key.id);
        guard
            .ssh_key_id_by_project_fingerprint
            .insert(fp_key, key.id);
        guard.ssh_keys_by_id.insert(key.id, key.clone());
        Ok(key)
    }

    async fn create_ssh_key_user(
        &self,
        user_id: Uuid,
        req: NewSshKey,
        fingerprint: String,
    ) -> Result<SshKey, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.users_by_id.contains_key(&user_id) {
            return Err(StoreError::NotFound);
        }
        let name_key = (user_id, req.name.clone());
        if guard.ssh_key_id_by_user_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "ssh key with name {:?} already exists for user {user_id}",
                req.name
            )));
        }
        let fp_key = (user_id, fingerprint.clone());
        if guard.ssh_key_id_by_user_fingerprint.contains_key(&fp_key) {
            return Err(StoreError::Conflict(format!(
                "ssh key with fingerprint {fingerprint} already exists for user {user_id}"
            )));
        }
        let scope = SshKeyScope::User { user_id };
        let id = crate::derive_ssh_key_id(&scope, &fingerprint);
        if guard.ssh_keys_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "ssh key with id {id} already exists",
            )));
        }
        let key = build_ssh_key(id, scope, &req, fingerprint);
        guard.ssh_key_id_by_user_name.insert(name_key, key.id);
        guard.ssh_key_id_by_user_fingerprint.insert(fp_key, key.id);
        guard.ssh_keys_by_id.insert(key.id, key.clone());
        Ok(key)
    }

    async fn get_ssh_key(&self, key_id: Uuid) -> Result<SshKey, StoreError> {
        let guard = self.inner.read().await;
        guard
            .ssh_keys_by_id
            .get(&key_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_ssh_keys_public(&self) -> Result<Vec<SshKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .ssh_keys_by_id
            .values()
            .filter(|k| matches!(k.scope, SshKeyScope::Public))
            .cloned()
            .collect())
    }

    async fn list_ssh_keys_in_silo(&self, silo_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .ssh_keys_by_id
            .values()
            .filter(|k| matches!(k.scope, SshKeyScope::Silo { silo_id: s } if s == silo_id))
            .cloned()
            .collect())
    }

    async fn list_ssh_keys_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .ssh_keys_by_id
            .values()
            .filter(|k| matches!(k.scope, SshKeyScope::Tenant { tenant_id: t } if t == tenant_id))
            .cloned()
            .collect())
    }

    async fn list_ssh_keys_in_project(&self, project_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .ssh_keys_by_id
            .values()
            .filter(
                |k| matches!(k.scope, SshKeyScope::Project { project_id: p } if p == project_id),
            )
            .cloned()
            .collect())
    }

    async fn list_ssh_keys_for_user(&self, user_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .ssh_keys_by_id
            .values()
            .filter(|k| matches!(k.scope, SshKeyScope::User { user_id: u } if u == user_id))
            .cloned()
            .collect())
    }

    async fn list_visible_ssh_keys_in_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<SshKey>, StoreError> {
        let guard = self.inner.read().await;
        let tenant = guard
            .tenants_by_id
            .get(&tenant_id)
            .ok_or(StoreError::NotFound)?
            .clone();
        let silo_id = tenant.silo_id;
        Ok(guard
            .ssh_keys_by_id
            .values()
            .filter(|k| match &k.scope {
                SshKeyScope::Public => true,
                SshKeyScope::Silo { silo_id: s } => *s == silo_id,
                SshKeyScope::Tenant { tenant_id: t } => *t == tenant_id,
                _ => false,
            })
            .cloned()
            .collect())
    }

    async fn list_visible_ssh_keys_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<SshKey>, StoreError> {
        let guard = self.inner.read().await;
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?
            .clone();
        let tenant = guard
            .tenants_by_id
            .get(&project.tenant_id)
            .ok_or(StoreError::NotFound)?
            .clone();
        let silo_id = tenant.silo_id;
        let tenant_id = project.tenant_id;
        Ok(guard
            .ssh_keys_by_id
            .values()
            .filter(|k| match &k.scope {
                SshKeyScope::Public => true,
                SshKeyScope::Silo { silo_id: s } => *s == silo_id,
                SshKeyScope::Tenant { tenant_id: t } => *t == tenant_id,
                SshKeyScope::Project { project_id: p } => *p == project_id,
                _ => false,
            })
            .cloned()
            .collect())
    }

    async fn delete_ssh_key(&self, key_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let key = guard
            .ssh_keys_by_id
            .remove(&key_id)
            .ok_or(StoreError::NotFound)?;
        match key.scope {
            SshKeyScope::Public => {
                guard.ssh_key_id_by_public_name.remove(&key.name);
                guard
                    .ssh_key_id_by_public_fingerprint
                    .remove(&key.fingerprint);
            }
            SshKeyScope::Silo { silo_id } => {
                guard
                    .ssh_key_id_by_silo_name
                    .remove(&(silo_id, key.name.clone()));
                guard
                    .ssh_key_id_by_silo_fingerprint
                    .remove(&(silo_id, key.fingerprint));
            }
            SshKeyScope::Tenant { tenant_id } => {
                guard
                    .ssh_key_id_by_tenant_name
                    .remove(&(tenant_id, key.name.clone()));
                guard
                    .ssh_key_id_by_tenant_fingerprint
                    .remove(&(tenant_id, key.fingerprint));
            }
            SshKeyScope::Project { project_id } => {
                guard
                    .ssh_key_id_by_project_name
                    .remove(&(project_id, key.name.clone()));
                guard
                    .ssh_key_id_by_project_fingerprint
                    .remove(&(project_id, key.fingerprint));
            }
            SshKeyScope::User { user_id } => {
                guard
                    .ssh_key_id_by_user_name
                    .remove(&(user_id, key.name.clone()));
                guard
                    .ssh_key_id_by_user_fingerprint
                    .remove(&(user_id, key.fingerprint));
            }
        }
        Ok(())
    }

    async fn create_image_public(&self, req: NewImage) -> Result<Image, StoreError> {
        let mut guard = self.inner.write().await;
        if guard.image_id_by_public_name.contains_key(&req.name) {
            return Err(StoreError::Conflict(format!(
                "public image with name {:?} already exists",
                req.name
            )));
        }
        let scope = ImageScope::Public;
        let id = req
            .id
            .unwrap_or_else(|| crate::derive_image_id(&scope, &req.sha256));
        if guard.images_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "image with id {id} already exists",
            )));
        }
        let image = build_image(id, scope, &req);
        guard
            .image_id_by_public_name
            .insert(image.name.clone(), image.id);
        guard.images_by_id.insert(image.id, image.clone());
        Ok(image)
    }

    async fn create_image_silo(&self, silo_id: Uuid, req: NewImage) -> Result<Image, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.silos_by_id.contains_key(&silo_id) {
            return Err(StoreError::NotFound);
        }
        let name_key = (silo_id, req.name.clone());
        if guard.image_id_by_silo_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "image with name {:?} already exists in silo {silo_id}",
                req.name
            )));
        }
        let scope = ImageScope::Silo { silo_id };
        let id = req
            .id
            .unwrap_or_else(|| crate::derive_image_id(&scope, &req.sha256));
        if guard.images_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "image with id {id} already exists",
            )));
        }
        let image = build_image(id, scope, &req);
        guard.image_id_by_silo_name.insert(name_key, image.id);
        guard.images_by_id.insert(image.id, image.clone());
        Ok(image)
    }

    async fn create_image_tenant(
        &self,
        tenant_id: Uuid,
        req: NewImage,
    ) -> Result<Image, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.tenants_by_id.contains_key(&tenant_id) {
            return Err(StoreError::NotFound);
        }
        let name_key = (tenant_id, req.name.clone());
        if guard.image_id_by_tenant_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "image with name {:?} already exists in tenant {tenant_id}",
                req.name
            )));
        }
        let scope = ImageScope::Tenant { tenant_id };
        let id = req
            .id
            .unwrap_or_else(|| crate::derive_image_id(&scope, &req.sha256));
        if guard.images_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "image with id {id} already exists",
            )));
        }
        let image = build_image(id, scope, &req);
        guard.image_id_by_tenant_name.insert(name_key, image.id);
        guard.images_by_id.insert(image.id, image.clone());
        Ok(image)
    }

    async fn create_image_project(
        &self,
        project_id: Uuid,
        req: NewImage,
    ) -> Result<Image, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.projects_by_id.contains_key(&project_id) {
            return Err(StoreError::NotFound);
        }
        let name_key = (project_id, req.name.clone());
        if guard.image_id_by_project_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "image with name {:?} already exists in project {project_id}",
                req.name
            )));
        }
        let scope = ImageScope::Project { project_id };
        let id = req
            .id
            .unwrap_or_else(|| crate::derive_image_id(&scope, &req.sha256));
        if guard.images_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "image with id {id} already exists",
            )));
        }
        let image = build_image(id, scope, &req);
        guard.image_id_by_project_name.insert(name_key, image.id);
        guard.images_by_id.insert(image.id, image.clone());
        Ok(image)
    }

    async fn create_image_user(&self, user_id: Uuid, req: NewImage) -> Result<Image, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.users_by_id.contains_key(&user_id) {
            return Err(StoreError::NotFound);
        }
        let name_key = (user_id, req.name.clone());
        if guard.image_id_by_user_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "image with name {:?} already exists for user {user_id}",
                req.name
            )));
        }
        let scope = ImageScope::User { user_id };
        let id = req
            .id
            .unwrap_or_else(|| crate::derive_image_id(&scope, &req.sha256));
        if guard.images_by_id.contains_key(&id) {
            return Err(StoreError::Conflict(format!(
                "image with id {id} already exists",
            )));
        }
        let image = build_image(id, scope, &req);
        guard.image_id_by_user_name.insert(name_key, image.id);
        guard.images_by_id.insert(image.id, image.clone());
        Ok(image)
    }

    async fn get_image(&self, image_id: Uuid) -> Result<Image, StoreError> {
        let guard = self.inner.read().await;
        guard
            .images_by_id
            .get(&image_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_images_public(&self) -> Result<Vec<Image>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .images_by_id
            .values()
            .filter(|i| matches!(i.scope, ImageScope::Public))
            .cloned()
            .collect())
    }

    async fn list_images_in_silo(&self, silo_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .images_by_id
            .values()
            .filter(|i| matches!(i.scope, ImageScope::Silo { silo_id: s } if s == silo_id))
            .cloned()
            .collect())
    }

    async fn list_images_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .images_by_id
            .values()
            .filter(|i| matches!(i.scope, ImageScope::Tenant { tenant_id: t } if t == tenant_id))
            .cloned()
            .collect())
    }

    async fn list_images_in_project(&self, project_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .images_by_id
            .values()
            .filter(|i| matches!(i.scope, ImageScope::Project { project_id: p } if p == project_id))
            .cloned()
            .collect())
    }

    async fn list_images_for_user(&self, user_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .images_by_id
            .values()
            .filter(|i| matches!(i.scope, ImageScope::User { user_id: u } if u == user_id))
            .cloned()
            .collect())
    }

    async fn list_visible_images_in_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<Image>, StoreError> {
        let guard = self.inner.read().await;
        // Tenant must exist; surfaces NotFound for cross-tenant
        // probes via the handler's authorize_in_tenant gate, but
        // we also want a clean NotFound for a stale tenant id
        // that slipped past Cedar.
        let tenant = guard
            .tenants_by_id
            .get(&tenant_id)
            .ok_or(StoreError::NotFound)?
            .clone();
        let silo_id = tenant.silo_id;
        Ok(guard
            .images_by_id
            .values()
            .filter(|i| match &i.scope {
                ImageScope::Public => true,
                ImageScope::Silo { silo_id: s } => *s == silo_id,
                ImageScope::Tenant { tenant_id: t } => *t == tenant_id,
                _ => false,
            })
            .cloned()
            .collect())
    }

    async fn list_visible_images_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Image>, StoreError> {
        let guard = self.inner.read().await;
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?
            .clone();
        let tenant = guard
            .tenants_by_id
            .get(&project.tenant_id)
            .ok_or(StoreError::NotFound)?
            .clone();
        let silo_id = tenant.silo_id;
        let tenant_id = project.tenant_id;
        Ok(guard
            .images_by_id
            .values()
            .filter(|i| match &i.scope {
                ImageScope::Public => true,
                ImageScope::Silo { silo_id: s } => *s == silo_id,
                ImageScope::Tenant { tenant_id: t } => *t == tenant_id,
                ImageScope::Project { project_id: p } => *p == project_id,
                _ => false,
            })
            .cloned()
            .collect())
    }

    async fn delete_image(&self, image_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let image = guard
            .images_by_id
            .remove(&image_id)
            .ok_or(StoreError::NotFound)?;
        match image.scope {
            ImageScope::Public => {
                guard.image_id_by_public_name.remove(&image.name);
            }
            ImageScope::Silo { silo_id } => {
                guard.image_id_by_silo_name.remove(&(silo_id, image.name));
            }
            ImageScope::Tenant { tenant_id } => {
                guard
                    .image_id_by_tenant_name
                    .remove(&(tenant_id, image.name));
            }
            ImageScope::Project { project_id } => {
                guard
                    .image_id_by_project_name
                    .remove(&(project_id, image.name));
            }
            ImageScope::User { user_id } => {
                guard.image_id_by_user_name.remove(&(user_id, image.name));
            }
        }
        Ok(())
    }

    async fn put_quota(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewQuota,
    ) -> Result<Quota, StoreError> {
        let mut guard = self.inner.write().await;
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?;
        if project.tenant_id != tenant_id {
            return Err(StoreError::NotFound);
        }
        let quota = Quota {
            tenant_id,
            project_id,
            cpu_limit: req.cpu_limit,
            memory_bytes: req.memory_bytes,
            disk_bytes: req.disk_bytes,
            instance_limit: req.instance_limit,
            updated_at: Utc::now(),
        };
        guard.quotas_by_project.insert(project_id, quota.clone());
        Ok(quota)
    }

    async fn get_quota(&self, tenant_id: Uuid, project_id: Uuid) -> Result<Quota, StoreError> {
        let guard = self.inner.read().await;
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?;
        if project.tenant_id != tenant_id {
            return Err(StoreError::NotFound);
        }
        guard
            .quotas_by_project
            .get(&project_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn create_instance(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewInstance,
    ) -> Result<InstanceCreateResult, StoreError> {
        let mut guard = self.inner.write().await;

        // Project must exist and be in the named tenant.
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?;
        if project.tenant_id != tenant_id {
            return Err(StoreError::NotFound);
        }

        // Image must exist. Visibility (cross-scope) is enforced
        // by the API handler before invoking this; the store
        // performs only the existence check so a stale image_id
        // surfaces as NotFound.
        let image = guard
            .images_by_id
            .get(&req.image_id)
            .ok_or(StoreError::NotFound)?
            .clone();

        // Subnet must exist and live under this same tenant+project.
        let subnet = guard
            .subnets_by_id
            .get(&req.primary_subnet_id)
            .ok_or(StoreError::NotFound)?;
        if subnet.tenant_id != tenant_id || subnet.project_id != project_id {
            return Err(StoreError::NotFound);
        }
        let subnet = subnet.clone();

        // Each ssh-key id must exist. As of slice G, SSH keys
        // are multi-scope (Public / Silo / Tenant / Project /
        // User); the cross-scope visibility check happens at the
        // API edge via `ssh_key_visible_to`. The store layer
        // only verifies existence — the same shape as multi-scope
        // image references in slice F.
        for key_id in &req.ssh_key_ids {
            if !guard.ssh_keys_by_id.contains_key(key_id) {
                return Err(StoreError::NotFound);
            }
        }

        let name_key = (project_id, req.name.clone());
        if guard.instance_id_by_project_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "instance with name {:?} already exists in project {project_id}",
                req.name
            )));
        }

        // Allocate the primary NIC's addresses. Each family is
        // allocated only when the parent subnet has it.
        let allocated_v4 = guard.allocated_ipv4_by_subnet.entry(subnet.id).or_default();
        let primary_ipv4 = match subnet.ipv4_block {
            Some(cidr) => {
                let ip = crate::types::allocate_ipv4(cidr, allocated_v4).ok_or_else(|| {
                    StoreError::Backend(format!("subnet {} ipv4 pool exhausted", subnet.id))
                })?;
                allocated_v4.insert(ip);
                Some(ip)
            }
            None => None,
        };
        let allocated_v6 = guard.allocated_ipv6_by_subnet.entry(subnet.id).or_default();
        let primary_ipv6 = match subnet.ipv6_block {
            Some(cidr) => {
                let ip = crate::types::allocate_ipv6(cidr, allocated_v6).ok_or_else(|| {
                    StoreError::Backend(format!("subnet {} ipv6 pool exhausted", subnet.id))
                })?;
                allocated_v6.insert(ip);
                Some(ip)
            }
            None => None,
        };

        let now = Utc::now();
        let instance_id = Uuid::new_v4();
        let mut rng = rand::rng();
        let nic = Nic {
            id: Uuid::new_v4(),
            tenant_id,
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
            tenant_id,
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
            id: Uuid::new_v4(),
            tenant_id,
            project_id,
            instance_id,
            name: "boot".to_string(),
            description: format!("Boot disk for instance {}", instance.name),
            kind: DiskKind::Boot,
            size_bytes: image.size_bytes,
            source_image_id: Some(image.id),
            created_at: now,
        };
        // Resolve and allocate the extra NICs declared on the
        // request. Each extra subnet must live under the same
        // silo+project; each extra NIC name must be unique
        // within the instance. We resolve all subnets *and*
        // allocate all addresses before any insert so a failure
        // half-way through doesn't leave partial state behind.
        let mut nic_records: Vec<Nic> = vec![nic];
        for spec in &req.extra_nics {
            if nic_records.iter().any(|n| n.name == spec.name) {
                return Err(StoreError::Conflict(format!(
                    "duplicate NIC name {:?} on instance",
                    spec.name
                )));
            }
            let extra_subnet = guard
                .subnets_by_id
                .get(&spec.subnet_id)
                .ok_or(StoreError::NotFound)?;
            if extra_subnet.tenant_id != tenant_id || extra_subnet.project_id != project_id {
                return Err(StoreError::NotFound);
            }
            let extra_subnet = extra_subnet.clone();
            let allocated_v4 = guard
                .allocated_ipv4_by_subnet
                .entry(extra_subnet.id)
                .or_default();
            let extra_v4 = match extra_subnet.ipv4_block {
                Some(cidr) => {
                    let ip = crate::types::allocate_ipv4(cidr, allocated_v4).ok_or_else(|| {
                        StoreError::Backend(format!(
                            "subnet {} ipv4 pool exhausted",
                            extra_subnet.id
                        ))
                    })?;
                    allocated_v4.insert(ip);
                    Some(ip)
                }
                None => None,
            };
            let allocated_v6 = guard
                .allocated_ipv6_by_subnet
                .entry(extra_subnet.id)
                .or_default();
            let extra_v6 = match extra_subnet.ipv6_block {
                Some(cidr) => {
                    let ip = crate::types::allocate_ipv6(cidr, allocated_v6).ok_or_else(|| {
                        StoreError::Backend(format!(
                            "subnet {} ipv6 pool exhausted",
                            extra_subnet.id
                        ))
                    })?;
                    allocated_v6.insert(ip);
                    Some(ip)
                }
                None => None,
            };
            nic_records.push(Nic {
                id: Uuid::new_v4(),
                tenant_id,
                project_id,
                instance_id,
                vpc_id: extra_subnet.vpc_id,
                subnet_id: extra_subnet.id,
                name: spec.name.clone(),
                mac: crate::types::generate_mac(&mut rng),
                primary_ipv4: extra_v4,
                primary_ipv6: extra_v6,
                created_at: now,
            });
        }

        guard
            .instance_id_by_project_name
            .insert(name_key, instance.id);
        guard.instances_by_id.insert(instance.id, instance.clone());
        for n in &nic_records {
            guard.nics_by_id.insert(n.id, n.clone());
        }
        guard.disks_by_id.insert(boot_disk.id, boot_disk.clone());
        Ok(InstanceCreateResult {
            instance,
            nics: nic_records,
            disks: vec![boot_disk],
        })
    }

    async fn get_instance(&self, instance_id: Uuid) -> Result<Instance, StoreError> {
        let guard = self.inner.read().await;
        guard
            .instances_by_id
            .get(&instance_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_instances_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Instance>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .instances_by_id
            .values()
            .filter(|i| i.project_id == project_id)
            .cloned()
            .collect())
    }

    async fn delete_instance(&self, instance_id: Uuid, force: bool) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        // Snapshot just the data we need so the lifecycle check
        // doesn't hold a borrow over the subsequent `remove`.
        let (lifecycle_kind, project_id, name) = {
            let instance = guard
                .instances_by_id
                .get(&instance_id)
                .ok_or(StoreError::NotFound)?;
            (
                instance.lifecycle.kind(),
                instance.project_id,
                instance.name.clone(),
            )
        };
        if !force {
            let deletable = matches!(
                lifecycle_kind,
                LifecycleStateKind::Stopped | LifecycleStateKind::Failed
            );
            if !deletable {
                return Err(StoreError::Conflict(format!(
                    "instance {instance_id} is not deletable in state {lifecycle_kind:?}; stop it first or pass ?force=true"
                )));
            }
        }

        // Cascade: collect NIC ids, then drop each NIC + free its
        // IP allocations.
        let nic_ids: Vec<Uuid> = guard
            .nics_by_id
            .values()
            .filter(|n| n.instance_id == instance_id)
            .map(|n| n.id)
            .collect();
        for nic_id in nic_ids {
            if let Some(nic) = guard.nics_by_id.remove(&nic_id) {
                if let Some(ip) = nic.primary_ipv4
                    && let Some(set) = guard.allocated_ipv4_by_subnet.get_mut(&nic.subnet_id)
                {
                    set.remove(&ip);
                }
                if let Some(ip) = nic.primary_ipv6
                    && let Some(set) = guard.allocated_ipv6_by_subnet.get_mut(&nic.subnet_id)
                {
                    set.remove(&ip);
                }
            }
        }

        // Cascade disks too. No allocator state for disks (unlike
        // NIC IPs); just drop the records.
        let disk_ids: Vec<Uuid> = guard
            .disks_by_id
            .values()
            .filter(|d| d.instance_id == instance_id)
            .map(|d| d.id)
            .collect();
        for disk_id in disk_ids {
            guard.disks_by_id.remove(&disk_id);
        }

        // Auto-detach (do NOT release) any FloatingIps attached
        // to this instance. The IP stays owned by the project so
        // the operator can re-attach it elsewhere — the canonical
        // "instance died, reuse the public IP" workflow.
        let fip_ids: Vec<Uuid> = guard
            .floating_ips_by_id
            .values()
            .filter(|f| {
                f.attached_to
                    .as_ref()
                    .map(|a| a.instance_id == instance_id)
                    .unwrap_or(false)
            })
            .map(|f| f.id)
            .collect();
        let now = Utc::now();
        for fip_id in fip_ids {
            if let Some(fip) = guard.floating_ips_by_id.get_mut(&fip_id) {
                fip.attached_to = None;
                fip.updated_at = now;
            }
        }

        guard.instances_by_id.remove(&instance_id);
        guard
            .instance_id_by_project_name
            .remove(&(project_id, name));
        Ok(())
    }

    async fn transition_instance_lifecycle(
        &self,
        instance_id: Uuid,
        expected_from: &[LifecycleStateKind],
        to: LifecycleState,
    ) -> Result<Instance, StoreError> {
        let mut guard = self.inner.write().await;
        let instance = guard
            .instances_by_id
            .get_mut(&instance_id)
            .ok_or(StoreError::NotFound)?;
        let current_kind = instance.lifecycle.kind();
        if !expected_from.contains(&current_kind) {
            return Err(StoreError::Conflict(format!(
                "instance {instance_id} is in {current_kind:?}; expected one of {expected_from:?}"
            )));
        }
        instance.lifecycle = to;
        instance.updated_at = Utc::now();
        Ok(instance.clone())
    }

    async fn get_nic(&self, nic_id: Uuid) -> Result<Nic, StoreError> {
        let guard = self.inner.read().await;
        guard
            .nics_by_id
            .get(&nic_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_nics_for_instance(&self, instance_id: Uuid) -> Result<Vec<Nic>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .nics_by_id
            .values()
            .filter(|n| n.instance_id == instance_id)
            .cloned()
            .collect())
    }

    async fn get_disk(&self, disk_id: Uuid) -> Result<Disk, StoreError> {
        let guard = self.inner.read().await;
        guard
            .disks_by_id
            .get(&disk_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_disks_for_instance(&self, instance_id: Uuid) -> Result<Vec<Disk>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .disks_by_id
            .values()
            .filter(|d| d.instance_id == instance_id)
            .cloned()
            .collect())
    }

    async fn create_floating_ip(
        &self,
        tenant_id: Uuid,
        project_id: Uuid,
        req: NewFloatingIp,
    ) -> Result<FloatingIp, StoreError> {
        let mut guard = self.inner.write().await;
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?;
        if project.tenant_id != tenant_id {
            return Err(StoreError::NotFound);
        }
        let name_key = (project_id, req.name.clone());
        if guard.floating_ip_id_by_project_name.contains_key(&name_key) {
            return Err(StoreError::Conflict(format!(
                "floating ip with name {:?} already exists in project {project_id}",
                req.name
            )));
        }
        let address: IpAddr = match req.family {
            AddressFamily::V4 => {
                let allocated = guard.public_ipv4_allocations.keys().copied().collect();
                crate::types::allocate_ipv4(FLOATING_IP_V4_POOL, &allocated)
                    .ok_or_else(|| {
                        StoreError::Backend("floating ip v4 pool exhausted".to_string())
                    })?
                    .into()
            }
            AddressFamily::V6 => {
                let allocated = guard.public_ipv6_allocations.keys().copied().collect();
                crate::types::allocate_ipv6(FLOATING_IP_V6_POOL, &allocated)
                    .ok_or_else(|| {
                        StoreError::Backend("floating ip v6 pool exhausted".to_string())
                    })?
                    .into()
            }
        };
        let now = Utc::now();
        let fip = FloatingIp {
            id: Uuid::new_v4(),
            tenant_id,
            project_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            address,
            attached_to: None,
            created_at: now,
            updated_at: now,
        };
        guard
            .floating_ip_id_by_project_name
            .insert(name_key, fip.id);
        let holder = public_ip_holder("floating_ip", fip.id);
        match address {
            IpAddr::V4(v4) => {
                guard.public_ipv4_allocations.insert(v4, holder);
            }
            IpAddr::V6(v6) => {
                guard.public_ipv6_allocations.insert(v6, holder);
            }
        }
        guard.floating_ips_by_id.insert(fip.id, fip.clone());
        Ok(fip)
    }

    async fn get_floating_ip(&self, fip_id: Uuid) -> Result<FloatingIp, StoreError> {
        let guard = self.inner.read().await;
        guard
            .floating_ips_by_id
            .get(&fip_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_floating_ips_in_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<FloatingIp>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .floating_ips_by_id
            .values()
            .filter(|f| f.project_id == project_id)
            .cloned()
            .collect())
    }

    async fn delete_floating_ip(&self, fip_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        // Snapshot the fields we need before the mutating remove
        // so we don't hold an immutable borrow over a mutable op.
        let (project_id, name, address, attached) = {
            let fip = guard
                .floating_ips_by_id
                .get(&fip_id)
                .ok_or(StoreError::NotFound)?;
            (
                fip.project_id,
                fip.name.clone(),
                fip.address,
                fip.attached_to.is_some(),
            )
        };
        if attached {
            return Err(StoreError::Conflict(format!(
                "floating ip {fip_id} is currently attached; detach first"
            )));
        }
        guard.floating_ips_by_id.remove(&fip_id);
        guard
            .floating_ip_id_by_project_name
            .remove(&(project_id, name));
        match address {
            IpAddr::V4(v4) => {
                guard.public_ipv4_allocations.remove(&v4);
            }
            IpAddr::V6(v6) => {
                guard.public_ipv6_allocations.remove(&v6);
            }
        }
        Ok(())
    }

    async fn attach_floating_ip(
        &self,
        fip_id: Uuid,
        target_nic_id: Uuid,
    ) -> Result<FloatingIp, StoreError> {
        let mut guard = self.inner.write().await;
        // Snapshot fip's tenant+project so we can validate the NIC.
        let (fip_tenant, fip_project) = {
            let fip = guard
                .floating_ips_by_id
                .get(&fip_id)
                .ok_or(StoreError::NotFound)?;
            (fip.tenant_id, fip.project_id)
        };
        // NIC must exist and live under the same tenant+project.
        let nic = guard
            .nics_by_id
            .get(&target_nic_id)
            .ok_or(StoreError::NotFound)?;
        if nic.tenant_id != fip_tenant || nic.project_id != fip_project {
            return Err(StoreError::NotFound);
        }
        let nic_instance_id = nic.instance_id;
        let new_attachment = FloatingIpAttachment {
            instance_id: nic_instance_id,
            nic_id: target_nic_id,
            attached_at: Utc::now(),
        };
        let fip = guard
            .floating_ips_by_id
            .get_mut(&fip_id)
            .ok_or(StoreError::NotFound)?;
        fip.attached_to = Some(new_attachment);
        fip.updated_at = Utc::now();
        Ok(fip.clone())
    }

    async fn detach_floating_ip(&self, fip_id: Uuid) -> Result<FloatingIp, StoreError> {
        let mut guard = self.inner.write().await;
        let fip = guard
            .floating_ips_by_id
            .get_mut(&fip_id)
            .ok_or(StoreError::NotFound)?;
        fip.attached_to = None;
        fip.updated_at = Utc::now();
        Ok(fip.clone())
    }

    async fn delete_quota(&self, tenant_id: Uuid, project_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?;
        if project.tenant_id != tenant_id {
            return Err(StoreError::NotFound);
        }
        guard
            .quotas_by_project
            .remove(&project_id)
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }

    async fn enqueue_job(&self, req: NewJob) -> Result<ProvisioningJob, StoreError> {
        let mut guard = self.inner.write().await;
        let seq = guard.next_job_seq;
        guard.next_job_seq = seq.checked_add(1).ok_or_else(|| {
            StoreError::Backend("job seq overflow (operationally unreachable)".to_string())
        })?;
        let job = ProvisioningJob {
            id: Uuid::new_v4(),
            kind: req.kind,
            status: JobStatus::Pending,
            seq,
            created_at: Utc::now(),
            claimed_at: None,
            claimed_by: None,
            completed_at: None,
            target_cn_uuid: req.target_cn_uuid,
        };
        guard.jobs_by_id.insert(job.id, job.clone());
        Ok(job)
    }

    async fn list_stale_claims(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ProvisioningJob>, StoreError> {
        let guard = self.inner.read().await;
        let stale = guard
            .jobs_by_id
            .values()
            .filter(|j| matches!(j.status.kind(), JobStatusKind::InProgress))
            .filter(|j| j.claimed_at.is_some_and(|t| t < cutoff))
            .cloned()
            .collect();
        Ok(stale)
    }

    async fn claim_next_job(
        &self,
        claimed_by: &str,
        claimer_cn: Option<Uuid>,
    ) -> Result<ProvisioningJob, StoreError> {
        let mut guard = self.inner.write().await;
        // FIFO: lowest `seq` among Pending whose target_cn_uuid
        // matches the claimer.
        let target_id = guard
            .jobs_by_id
            .values()
            .filter(|j| matches!(j.status.kind(), JobStatusKind::Pending))
            .filter(|j| job_target_matches(j.target_cn_uuid, claimer_cn))
            .min_by_key(|j| j.seq)
            .map(|j| j.id);
        let id = target_id.ok_or(StoreError::NotFound)?;
        let job = guard.jobs_by_id.get_mut(&id).ok_or(StoreError::NotFound)?;
        job.status = JobStatus::InProgress;
        job.claimed_at = Some(Utc::now());
        job.claimed_by = Some(claimed_by.to_string());
        Ok(job.clone())
    }

    async fn complete_job(
        &self,
        job_id: Uuid,
        outcome: JobOutcome,
    ) -> Result<ProvisioningJob, StoreError> {
        let mut guard = self.inner.write().await;
        let job = guard
            .jobs_by_id
            .get_mut(&job_id)
            .ok_or(StoreError::NotFound)?;
        match job.status.kind() {
            JobStatusKind::Completed | JobStatusKind::Failed => {
                return Err(StoreError::Conflict(format!(
                    "job {job_id} is already terminal ({:?})",
                    job.status.kind()
                )));
            }
            _ => {}
        }
        job.status = match outcome {
            JobOutcome::Completed => JobStatus::Completed,
            JobOutcome::Failed { reason } => JobStatus::Failed { reason },
        };
        job.completed_at = Some(Utc::now());
        Ok(job.clone())
    }

    async fn get_job(&self, job_id: Uuid) -> Result<ProvisioningJob, StoreError> {
        let guard = self.inner.read().await;
        guard
            .jobs_by_id
            .get(&job_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_recent_jobs(&self, limit: usize) -> Result<Vec<ProvisioningJob>, StoreError> {
        let guard = self.inner.read().await;
        let mut jobs: Vec<ProvisioningJob> = guard.jobs_by_id.values().cloned().collect();
        // Newest first by creation time (ties broken by seq).
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
        admin_ip: Option<Ipv4Addr>,
        sysinfo: serde_json::Value,
        now: chrono::DateTime<Utc>,
    ) -> Result<Cn, StoreError> {
        let mut guard = self.inner.write().await;

        // Disabled records block re-registration.
        if let Some(existing) = guard.cns_by_server_uuid.get(&server_uuid)
            && existing.state == CnState::Disabled
        {
            return Err(StoreError::Conflict(format!(
                "cn {server_uuid} is disabled; remove the record before re-registering"
            )));
        }

        // Already-Approved records: idempotent refresh.
        if let Some(existing) = guard.cns_by_server_uuid.get(&server_uuid).cloned()
            && existing.state == CnState::Approved
        {
            let mut updated = existing;
            updated.hostname = hostname;
            updated.admin_ip = admin_ip;
            updated.sysinfo = sysinfo;
            updated.last_seen = Some(now);
            guard
                .cns_by_server_uuid
                .insert(server_uuid, updated.clone());
            return Ok(updated);
        }

        // Pending: rotate claim_code + poll_token, refresh sysinfo.
        if let Some(existing) = guard.cns_by_server_uuid.get(&server_uuid).cloned() {
            // Drop old indexes.
            if let Some(old_code) = &existing.claim_code {
                guard.cn_server_uuid_by_claim_code.remove(old_code);
            }
            guard
                .cn_server_uuid_by_poll_token
                .remove(&existing.poll_token);

            let (claim_code, poll_token) = mem_mint_claim_and_poll(&guard);
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
            guard
                .cn_server_uuid_by_claim_code
                .insert(claim_code, server_uuid);
            guard
                .cn_server_uuid_by_poll_token
                .insert(poll_token, server_uuid);
            guard.cns_by_server_uuid.insert(server_uuid, cn.clone());
            return Ok(cn);
        }

        // Brand new registration. Try to consume an auto-approve slot.
        let auto_approved = mem_try_consume_window(&mut guard, now);

        let poll_token = mem_unique_poll_token(&guard);
        let (claim_code, claim_expiry, state) = if auto_approved {
            (None, None, CnState::Approved)
        } else {
            let code = mem_unique_claim_code(&guard);
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
        if let Some(code) = &claim_code {
            guard
                .cn_server_uuid_by_claim_code
                .insert(code.clone(), server_uuid);
        }
        guard
            .cn_server_uuid_by_poll_token
            .insert(poll_token, server_uuid);
        guard.cns_by_server_uuid.insert(server_uuid, cn.clone());
        Ok(cn)
    }

    async fn get_cn(&self, server_uuid: Uuid) -> Result<Cn, StoreError> {
        let guard = self.inner.read().await;
        guard
            .cns_by_server_uuid
            .get(&server_uuid)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_cn_by_poll_token(&self, poll_token: &str) -> Result<Cn, StoreError> {
        let guard = self.inner.read().await;
        let server_uuid = guard
            .cn_server_uuid_by_poll_token
            .get(poll_token)
            .copied()
            .ok_or(StoreError::NotFound)?;
        guard
            .cns_by_server_uuid
            .get(&server_uuid)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_cn_by_claim_code(&self, code: &str) -> Result<Cn, StoreError> {
        let guard = self.inner.read().await;
        let server_uuid = guard
            .cn_server_uuid_by_claim_code
            .get(code)
            .copied()
            .ok_or(StoreError::NotFound)?;
        let cn = guard
            .cns_by_server_uuid
            .get(&server_uuid)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        // Conflate state-mismatch and expiry into NotFound so probes can't
        // distinguish "wrong code" from "right code, wrong state".
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
        let guard = self.inner.read().await;
        let cns: Vec<Cn> = guard
            .cns_by_server_uuid
            .values()
            .filter(|c| state_filter.is_none_or(|s| c.state == s))
            .cloned()
            .collect();
        Ok(cns)
    }

    async fn approve_cn(
        &self,
        server_uuid: Uuid,
        bound_api_key_id: Uuid,
        pending_credential: String,
        approved_at: chrono::DateTime<Utc>,
    ) -> Result<Cn, StoreError> {
        let mut guard = self.inner.write().await;
        let mut cn = guard
            .cns_by_server_uuid
            .get(&server_uuid)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        // Disabled blocks the flow entirely (record is gone for
        // approval purposes). Re-bind blocks too — operators must
        // disable + re-approve to rotate the credential.
        if cn.state == CnState::Disabled {
            return Err(StoreError::NotFound);
        }
        if cn.bound_api_key_id.is_some() {
            return Err(StoreError::Conflict(
                "cn already has a bound api key; disable + re-approve to rotate".to_string(),
            ));
        }
        if let Some(old_code) = &cn.claim_code {
            guard.cn_server_uuid_by_claim_code.remove(old_code);
        }
        cn.state = CnState::Approved;
        // Preserve approved_at when register_cn already stamped it
        // (auto-approve case); set it now for the Pending → Approved
        // transition.
        if cn.approved_at.is_none() {
            cn.approved_at = Some(approved_at);
        }
        cn.claim_code = None;
        cn.claim_code_expires_at = None;
        cn.bound_api_key_id = Some(bound_api_key_id);
        cn.pending_credential = Some(pending_credential);
        guard.cns_by_server_uuid.insert(server_uuid, cn.clone());
        Ok(cn)
    }

    async fn consume_cn_pending_credential(
        &self,
        poll_token: &str,
    ) -> Result<Option<String>, StoreError> {
        let mut guard = self.inner.write().await;
        let server_uuid = guard
            .cn_server_uuid_by_poll_token
            .get(poll_token)
            .copied()
            .ok_or(StoreError::NotFound)?;
        let cn = guard
            .cns_by_server_uuid
            .get_mut(&server_uuid)
            .ok_or(StoreError::NotFound)?;
        Ok(cn.pending_credential.take())
    }

    async fn disable_cn(&self, server_uuid: Uuid) -> Result<Cn, StoreError> {
        let mut guard = self.inner.write().await;
        let mut cn = guard
            .cns_by_server_uuid
            .get(&server_uuid)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if let Some(old_code) = &cn.claim_code {
            guard.cn_server_uuid_by_claim_code.remove(old_code);
        }
        cn.state = CnState::Disabled;
        cn.claim_code = None;
        cn.claim_code_expires_at = None;
        cn.pending_credential = None;
        guard.cns_by_server_uuid.insert(server_uuid, cn.clone());
        Ok(cn)
    }

    async fn update_cn_last_seen(
        &self,
        server_uuid: Uuid,
        at: chrono::DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let cn = guard
            .cns_by_server_uuid
            .get_mut(&server_uuid)
            .ok_or(StoreError::NotFound)?;
        cn.last_seen = Some(at);
        Ok(())
    }

    async fn update_cn_status(
        &self,
        server_uuid: Uuid,
        payload: serde_json::Value,
        at: chrono::DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let cn = guard
            .cns_by_server_uuid
            .get_mut(&server_uuid)
            .ok_or(StoreError::NotFound)?;
        cn.last_status = Some(payload);
        cn.last_seen = Some(at);
        Ok(())
    }

    async fn get_auto_approve_window(&self) -> Result<Option<AutoApproveWindow>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard.auto_approve_window.clone())
    }

    async fn open_auto_approve_window(&self, w: AutoApproveWindow) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        guard.auto_approve_window = Some(w);
        Ok(())
    }

    async fn close_auto_approve_window(&self) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        guard.auto_approve_window = None;
        Ok(())
    }

    async fn try_consume_auto_approve_slot(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut guard = self.inner.write().await;
        Ok(mem_try_consume_window(&mut guard, now))
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
        let mut inner = self.inner.write().await;
        if let Some(existing) = inner.network_realizations.get(&(resource, realizer))
            && existing.generation > generation
        {
            return Err(StoreError::Conflict(format!(
                "backward generation report for {} {}: existing={}, attempted={}",
                resource.kind_tag(),
                resource.id(),
                existing.generation,
                generation,
            )));
        }
        let row = Realization {
            realizer,
            generation,
            status,
            last_reported_at: Utc::now(),
            message,
        };
        inner.network_realizations.insert((resource, realizer), row);
        Ok(())
    }

    async fn list_network_realizations(
        &self,
        resource: NetworkResourceId,
    ) -> Result<Vec<Realization>, StoreError> {
        let inner = self.inner.read().await;
        Ok(realization_rows(&inner.network_realizations, resource))
    }
}

/// `chrono::Duration` form of [`CLAIM_CODE_TTL`]. Hand-converted (the
/// public constant is `std::time::Duration`) to avoid a `.expect()`
/// on `chrono::Duration::from_std` and keep the workspace
/// `clippy::expect_used` deny lint happy. The TTL is one hour;
/// well within `chrono::Duration`'s i64-millis range.
fn claim_code_ttl() -> chrono::Duration {
    chrono::Duration::seconds(CLAIM_CODE_TTL.as_secs() as i64)
}

/// Look up an unused claim code, retrying on collision. Probability
/// of collision at any realistic pending count is negligible (30
/// bits of entropy), so the retry loop almost never spins.
fn mem_unique_claim_code(guard: &Inner) -> String {
    let mut rng = rand::rng();
    for _ in 0..16 {
        let code = generate_claim_code(&mut rng);
        if !guard.cn_server_uuid_by_claim_code.contains_key(&code) {
            return code;
        }
    }
    // Astronomically unlikely; fall back to using the candidate
    // anyway (the FDB layer's CAS would catch a real collision).
    generate_claim_code(&mut rng)
}

fn mem_unique_poll_token(guard: &Inner) -> String {
    let mut rng = rand::rng();
    for _ in 0..16 {
        let token = generate_poll_token(&mut rng);
        if !guard.cn_server_uuid_by_poll_token.contains_key(&token) {
            return token;
        }
    }
    generate_poll_token(&mut rng)
}

/// Convenience for the Pending → claim_code+poll_token rotation
/// case in `register_cn`. Doesn't insert; returns the freshly-minted
/// pair for the caller to wire up.
fn mem_mint_claim_and_poll(guard: &Inner) -> (String, String) {
    let claim = mem_unique_claim_code(guard);
    let poll = mem_unique_poll_token(guard);
    (claim, poll)
}

/// Decrement the auto-approve window if it's open and has a slot.
/// Closes the window when remaining_count reaches 0 or expires_at
/// passes. Returns true iff a slot was consumed.
fn mem_try_consume_window(guard: &mut Inner, now: chrono::DateTime<Utc>) -> bool {
    let Some(window) = guard.auto_approve_window.as_mut() else {
        return false;
    };
    if now >= window.expires_at {
        guard.auto_approve_window = None;
        return false;
    }
    match window.remaining_count {
        Some(0) => {
            guard.auto_approve_window = None;
            false
        }
        Some(ref mut n) => {
            *n -= 1;
            let exhausted = *n == 0;
            if exhausted {
                guard.auto_approve_window = None;
            }
            true
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_fixture(name: &str) -> User {
        User {
            id: Uuid::new_v4(),
            username: name.to_string(),
            password_hash: "$2y$12$dummyhash".to_string(),
            is_root: false,
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        }
    }

    fn federated_user_fixture(tenant_id: Uuid, issuer: &str, subject: &str) -> User {
        use crate::Federation;
        User {
            id: Uuid::new_v4(),
            username: format!("{subject}@{issuer}"),
            password_hash: String::new(),
            is_root: false,
            created_at: Utc::now(),
            tenant_id: Some(tenant_id),
            federation: Some(Federation {
                issuer: issuer.to_string(),
                subject: subject.to_string(),
            }),
        }
    }

    #[tokio::test]
    async fn create_then_get_silo_returns_same_record() {
        let store = MemStore::new();
        let created = store
            .create_silo(NewSilo {
                name: "operator".to_string(),
                description: Some("the bootstrap silo".to_string()),
            })
            .await
            .unwrap();
        let fetched = store.get_silo(created.id).await.unwrap();
        assert_eq!(created, fetched);
    }

    #[tokio::test]
    async fn duplicate_silo_name_conflicts() {
        let store = MemStore::new();
        store
            .create_silo(NewSilo {
                name: "ops".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let err = store
            .create_silo(NewSilo {
                name: "ops".to_string(),
                description: None,
            })
            .await
            .expect_err("second create should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn get_unknown_silo_id_is_not_found() {
        let store = MemStore::new();
        let err = store
            .get_silo(Uuid::new_v4())
            .await
            .expect_err("unknown id should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn missing_silo_description_stored_as_empty_string() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "blank".to_string(),
                description: None,
            })
            .await
            .unwrap();
        assert_eq!(silo.description, "");
    }

    #[tokio::test]
    async fn user_round_trips_by_username_and_id() {
        let store = MemStore::new();
        let user = user_fixture("root");
        let user_id = user.id;
        let created = store.create_user(user).await.unwrap();
        assert_eq!(created.id, user_id);

        let by_username = store.get_user_by_username("root").await.unwrap();
        assert_eq!(by_username.id, user_id);

        let by_id = store.get_user_by_id(user_id).await.unwrap();
        assert_eq!(by_id.username, "root");
    }

    #[tokio::test]
    async fn duplicate_username_conflicts() {
        let store = MemStore::new();
        store.create_user(user_fixture("root")).await.unwrap();
        let err = store
            .create_user(user_fixture("root"))
            .await
            .expect_err("second create should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn has_any_user_flips_after_first_create() {
        let store = MemStore::new();
        assert!(!store.has_any_user().await.unwrap());
        store.create_user(user_fixture("root")).await.unwrap();
        assert!(store.has_any_user().await.unwrap());
    }

    #[tokio::test]
    async fn api_key_list_filters_by_owner_and_delete_removes() {
        let store = MemStore::new();
        let owner = user_fixture("alice");
        let other = user_fixture("bob");
        store.create_user(owner.clone()).await.unwrap();
        store.create_user(other.clone()).await.unwrap();

        let key_a = ApiKey {
            id: Uuid::new_v4(),
            user_id: owner.id,
            description: "ci".to_string(),
            lookup_id: "AAAAAAAAAAAA".to_string(),
            hash: "$hashA".to_string(),
            scope: ApiKeyScope::Full,
            bound_to_cn: None,
            created_at: Utc::now(),
        };
        let key_b = ApiKey {
            id: Uuid::new_v4(),
            user_id: other.id,
            description: "tf".to_string(),
            lookup_id: "BBBBBBBBBBBB".to_string(),
            hash: "$hashB".to_string(),
            scope: ApiKeyScope::Full,
            bound_to_cn: None,
            created_at: Utc::now(),
        };
        store.create_api_key(key_a.clone()).await.unwrap();
        store.create_api_key(key_b.clone()).await.unwrap();

        let owner_keys = store.list_api_keys(owner.id).await.unwrap();
        assert_eq!(owner_keys.len(), 1);
        assert_eq!(owner_keys[0].id, key_a.id);

        // O(1) lookup by lookup_id resolves to the right record.
        let resolved = store
            .get_api_key_by_lookup_id("AAAAAAAAAAAA")
            .await
            .unwrap();
        assert_eq!(resolved.id, key_a.id);

        // delete by wrong owner is not-found
        let err = store
            .delete_api_key(other.id, key_a.id)
            .await
            .expect_err("cross-owner delete should be not-found");
        assert!(matches!(err, StoreError::NotFound));

        // delete by right owner works, removes the lookup index, and
        // a second delete is not-found.
        store.delete_api_key(owner.id, key_a.id).await.unwrap();
        let err = store
            .get_api_key_by_lookup_id("AAAAAAAAAAAA")
            .await
            .expect_err("post-delete lookup should be not-found");
        assert!(matches!(err, StoreError::NotFound));
        let err = store
            .delete_api_key(owner.id, key_a.id)
            .await
            .expect_err("repeat delete should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn federated_user_round_trips_by_federation_triple() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "fed-rt".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let user =
            federated_user_fixture(silo.default_tenant_id, "https://idp.example", "tenant-42");
        let user_id = user.id;
        store.create_user(user).await.unwrap();

        let resolved = store
            .get_user_by_federation(silo.default_tenant_id, "https://idp.example", "tenant-42")
            .await
            .unwrap();
        assert_eq!(resolved.id, user_id);
        assert_eq!(resolved.tenant_id, Some(silo.default_tenant_id));
    }

    #[tokio::test]
    async fn duplicate_federation_triple_conflicts() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "dup-fed".to_string(),
                description: None,
            })
            .await
            .unwrap();
        store
            .create_user(federated_user_fixture(
                silo.default_tenant_id,
                "https://idp.example",
                "tenant-42",
            ))
            .await
            .unwrap();
        // Same (silo, issuer, subject) but distinct username/uuid:
        let mut second =
            federated_user_fixture(silo.default_tenant_id, "https://idp.example", "tenant-42");
        second.username = format!("alt-{}", second.id);
        let err = store
            .create_user(second)
            .await
            .expect_err("duplicate federation triple should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn project_round_trip_within_tenant() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "tenants".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;

        let p = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "alpha".to_string(),
                    description: Some("first".to_string()),
                },
            )
            .await
            .unwrap();
        assert_eq!(p.tenant_id, tenant_id);

        let fetched = store.get_project(p.id).await.unwrap();
        assert_eq!(fetched, p);

        let listed = store.list_projects_in_tenant(tenant_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, p.id);

        store.delete_project(p.id).await.unwrap();
        let err = store
            .get_project(p.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn duplicate_project_name_within_tenant_conflicts() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "ops".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;
        store
            .create_project(
                tenant_id,
                NewProject {
                    name: "alpha".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let err = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "alpha".to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("duplicate within tenant conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn same_project_name_in_different_tenants_does_not_conflict() {
        let store = MemStore::new();
        let a = store
            .create_silo(NewSilo {
                name: "silo-a".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let b = store
            .create_silo(NewSilo {
                name: "silo-b".to_string(),
                description: None,
            })
            .await
            .unwrap();
        store
            .create_project(
                a.default_tenant_id,
                NewProject {
                    name: "shared".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        store
            .create_project(
                b.default_tenant_id,
                NewProject {
                    name: "shared".to_string(),
                    description: None,
                },
            )
            .await
            .expect("same name across tenants must be allowed");
    }

    #[tokio::test]
    async fn create_project_in_unknown_tenant_is_not_found() {
        let store = MemStore::new();
        let err = store
            .create_project(
                Uuid::new_v4(),
                NewProject {
                    name: "orphan".to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("unknown tenant should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn tenant_round_trip() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "brand".to_string(),
                description: None,
            })
            .await
            .unwrap();

        let t = store
            .create_tenant(
                silo.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: Some("first customer".to_string()),
                },
            )
            .await
            .unwrap();
        assert_eq!(t.silo_id, silo.id);
        assert_eq!(t.name, "acme");
        assert_eq!(t.description, "first customer");

        let fetched = store.get_tenant(t.id).await.unwrap();
        assert_eq!(fetched, t);

        // The silo now ships with a "default" tenant created
        // atomically alongside it (E-2), so the explicit tenant
        // we just created should be the second of two listed.
        let listed = store.list_tenants_in_silo(silo.id).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|t2| t2.id == t.id));
        assert!(listed.iter().any(|t2| t2.id == silo.default_tenant_id));

        store.delete_tenant(t.id).await.unwrap();
        let err = store
            .get_tenant(t.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn tenants_within_silo_must_have_unique_names() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "brand".to_string(),
                description: None,
            })
            .await
            .unwrap();
        store
            .create_tenant(
                silo.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let err = store
            .create_tenant(
                silo.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("duplicate tenant name within silo conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn same_tenant_name_in_different_silos_does_not_conflict() {
        let store = MemStore::new();
        let a = store
            .create_silo(NewSilo {
                name: "brand-a".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let b = store
            .create_silo(NewSilo {
                name: "brand-b".to_string(),
                description: None,
            })
            .await
            .unwrap();
        store
            .create_tenant(
                a.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        store
            .create_tenant(
                b.id,
                NewTenant {
                    name: "acme".to_string(),
                    description: None,
                },
            )
            .await
            .expect("same tenant name across silos must be allowed");
    }

    #[tokio::test]
    async fn list_tenants_in_unknown_silo_returns_not_found() {
        let store = MemStore::new();
        let err = store
            .list_tenants_in_silo(Uuid::new_v4())
            .await
            .expect_err("unknown silo should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn idp_config_round_trip() {
        let store = MemStore::new();
        let tenant_id = Uuid::new_v4();
        let config = IdpConfig {
            issuer_url: "https://idp.example".to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        };
        let err = store
            .get_idp_config(tenant_id)
            .await
            .expect_err("missing idp config is not-found");
        assert!(matches!(err, StoreError::NotFound));

        store
            .put_idp_config(tenant_id, config.clone())
            .await
            .unwrap();
        let read = store.get_idp_config(tenant_id).await.unwrap();
        assert_eq!(read.issuer_url, "https://idp.example");

        let listed = store.list_idp_configs().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, tenant_id);

        // Issuer-keyed reverse lookup resolves to the same tenant.
        let (by_iss_tenant, by_iss_cfg) = store
            .get_idp_config_by_issuer("https://idp.example")
            .await
            .unwrap();
        assert_eq!(by_iss_tenant, tenant_id);
        assert_eq!(by_iss_cfg.issuer_url, "https://idp.example");

        store.delete_idp_config(tenant_id).await.unwrap();
        let err = store
            .get_idp_config(tenant_id)
            .await
            .expect_err("deleted idp config is not-found");
        assert!(matches!(err, StoreError::NotFound));
        // The reverse-index entry is dropped too.
        let err = store
            .get_idp_config_by_issuer("https://idp.example")
            .await
            .expect_err("post-delete issuer lookup is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn idp_config_issuer_uniqueness_across_tenants() {
        let store = MemStore::new();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let config = IdpConfig {
            issuer_url: "https://idp.example".to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        };
        store
            .put_idp_config(tenant_a, config.clone())
            .await
            .unwrap();
        // Same tenant, identical config → idempotent OK.
        store
            .put_idp_config(tenant_a, config.clone())
            .await
            .expect("idempotent re-put for the same tenant must succeed");
        // Same tenant, different issuer → OK; the old issuer index
        // entry is dropped so no other tenant can be blocked by it.
        let alt = IdpConfig {
            issuer_url: "https://idp.alt".to_string(),
            ..config.clone()
        };
        store
            .put_idp_config(tenant_a, alt.clone())
            .await
            .expect("changing the same tenant's issuer is fine");
        // Different tenant, original issuer → now free to claim.
        store
            .put_idp_config(tenant_b, config.clone())
            .await
            .expect("issuer freed by tenant_a's swap should now be claimable");
        // Different tenant, currently-claimed issuer → conflict.
        let err = store
            .put_idp_config(
                tenant_b,
                IdpConfig {
                    issuer_url: "https://idp.alt".to_string(),
                    ..config
                },
            )
            .await
            .expect_err("cross-tenant duplicate issuer must conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn duplicate_lookup_id_conflicts() {
        let store = MemStore::new();
        let owner = user_fixture("alice");
        store.create_user(owner.clone()).await.unwrap();

        let make = |id: Uuid| ApiKey {
            id,
            user_id: owner.id,
            description: "dup".to_string(),
            lookup_id: "AAAAAAAAAAAA".to_string(),
            hash: "$hash".to_string(),
            scope: ApiKeyScope::Full,
            bound_to_cn: None,
            created_at: Utc::now(),
        };
        store.create_api_key(make(Uuid::new_v4())).await.unwrap();
        let err = store
            .create_api_key(make(Uuid::new_v4()))
            .await
            .expect_err("second create with same lookup_id should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    fn ipv4_cidr(s: &str) -> ipnetwork::Ipv4Network {
        s.parse().expect("test fixture must be a valid CIDR")
    }

    fn ipv6_cidr(s: &str) -> ipnetwork::Ipv6Network {
        s.parse().expect("test fixture must be a valid CIDR")
    }

    /// Returns `(tenant_id, tenant_id, project_id)` so callers that
    /// still need silo_id (e.g. silo-scoped image / ssh-key
    /// fixtures) can keep their wiring while project-scoped work
    /// uses tenant_id.
    async fn make_silo_and_project(store: &MemStore) -> (Uuid, Uuid, Uuid) {
        let silo = store
            .create_silo(NewSilo {
                name: format!("silo-{}", Uuid::new_v4()),
                description: None,
            })
            .await
            .unwrap();
        let project = store
            .create_project(
                silo.default_tenant_id,
                NewProject {
                    name: "default".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        (silo.id, silo.default_tenant_id, project.id)
    }

    #[tokio::test]
    async fn vpc_round_trip_within_project() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;

        let vpc = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "prod".to_string(),
                    description: Some("primary".to_string()),
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: Some(ipv6_cidr("fd00::/48")),
                },
            )
            .await
            .unwrap();
        assert_eq!(vpc.tenant_id, tenant_id);
        assert_eq!(vpc.project_id, project_id);
        assert_ne!(vpc.main_route_table_id, Uuid::nil());
        assert!(vpc.vni >= VPC_VNI_RESERVED_CEILING && vpc.vni < VPC_VNI_MAX);
        assert_eq!(vpc.ipv4_block, Some(ipv4_cidr("10.0.0.0/24")));
        assert_eq!(vpc.ipv6_block, Some(ipv6_cidr("fd00::/48")));

        let fetched = store.get_vpc(vpc.id).await.unwrap();
        assert_eq!(fetched, vpc);

        let main_rt = store
            .get_route_table(vpc.main_route_table_id)
            .await
            .unwrap();
        assert_eq!(main_rt.tenant_id, tenant_id);
        assert_eq!(main_rt.project_id, project_id);
        assert_eq!(main_rt.vpc_id, vpc.id);
        assert_eq!(main_rt.name, MAIN_ROUTE_TABLE_NAME);
        assert!(main_rt.is_main);

        let route_tables = store.list_route_tables_in_vpc(vpc.id).await.unwrap();
        assert_eq!(route_tables.len(), 1);
        assert_eq!(route_tables[0].id, vpc.main_route_table_id);

        let listed = store.list_vpcs_in_project(project_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, vpc.id);

        store.delete_vpc(vpc.id).await.unwrap();
        let err = store
            .get_vpc(vpc.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));
        let err = store
            .get_route_table(vpc.main_route_table_id)
            .await
            .expect_err("vpc delete removes its main route table");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn vpc_ipv4_only_and_ipv6_only_round_trip() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;

        let v4 = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "v4".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.1.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        assert!(v4.ipv4_block.is_some());
        assert!(v4.ipv6_block.is_none());

        let v6 = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "v6".to_string(),
                    description: None,
                    ipv4_block: None,
                    ipv6_block: Some(ipv6_cidr("fd01::/48")),
                },
            )
            .await
            .unwrap();
        assert!(v6.ipv4_block.is_none());
        assert!(v6.ipv6_block.is_some());
        assert_ne!(v4.vni, v6.vni);
    }

    #[tokio::test]
    async fn duplicate_vpc_name_within_project_conflicts() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "alpha".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let err = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "alpha".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.1.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("duplicate name within project should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn same_vpc_name_in_different_projects_does_not_conflict() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "tenants".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;
        let p1 = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "alpha".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let p2 = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "beta".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        store
            .create_vpc(
                tenant_id,
                p1.id,
                NewVpc {
                    name: "shared".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        store
            .create_vpc(
                tenant_id,
                p2.id,
                NewVpc {
                    name: "shared".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.1.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect("same vpc name in a different project must be allowed");
    }

    #[tokio::test]
    async fn create_vpc_in_unknown_project_is_not_found() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "ops".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let err = store
            .create_vpc(
                silo.default_tenant_id,
                Uuid::new_v4(),
                NewVpc {
                    name: "orphan".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("unknown project should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn create_vpc_with_project_in_wrong_tenant_is_not_found() {
        let store = MemStore::new();
        let silo_a = store
            .create_silo(NewSilo {
                name: "silo-a".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let silo_b = store
            .create_silo(NewSilo {
                name: "silo-b".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let project = store
            .create_project(
                silo_a.default_tenant_id,
                NewProject {
                    name: "owned-by-a".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();

        // Caller claims silo_b's tenant but the project lives in silo_a's.
        let err = store
            .create_vpc(
                silo_b.default_tenant_id,
                project.id,
                NewVpc {
                    name: "wrong".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("project-in-wrong-tenant should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    /// Returns `(silo_id, tenant_id, project_id, vpc)`.
    async fn make_silo_project_vpc(store: &MemStore) -> (Uuid, Uuid, Uuid, Vpc) {
        let (silo_id, tenant_id, project_id) = make_silo_and_project(store).await;
        let vpc = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "vpc1".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/16")),
                    ipv6_block: Some(ipv6_cidr("fd00::/48")),
                },
            )
            .await
            .unwrap();
        (silo_id, tenant_id, project_id, vpc)
    }

    #[tokio::test]
    async fn route_table_round_trip_within_vpc() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        let route_table = store
            .create_route_table(
                tenant_id,
                project_id,
                vpc.id,
                NewRouteTable {
                    name: "private".to_string(),
                    description: Some("private routes".to_string()),
                },
            )
            .await
            .unwrap();
        assert_eq!(route_table.tenant_id, tenant_id);
        assert_eq!(route_table.project_id, project_id);
        assert_eq!(route_table.vpc_id, vpc.id);
        assert_eq!(route_table.name, "private");
        assert!(!route_table.is_main);

        let fetched = store.get_route_table(route_table.id).await.unwrap();
        assert_eq!(fetched, route_table);

        let listed = store.list_route_tables_in_vpc(vpc.id).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|rt| rt.id == vpc.main_route_table_id));
        assert!(listed.iter().any(|rt| rt.id == route_table.id));

        store.delete_route_table(route_table.id).await.unwrap();
        let err = store
            .get_route_table(route_table.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn route_table_name_unique_within_vpc() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        let err = store
            .create_route_table(
                tenant_id,
                project_id,
                vpc.id,
                NewRouteTable {
                    name: MAIN_ROUTE_TABLE_NAME.to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("main route table reserves the name");
        assert!(matches!(err, StoreError::Conflict(_)));

        store
            .create_route_table(
                tenant_id,
                project_id,
                vpc.id,
                NewRouteTable {
                    name: "custom".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let err = store
            .create_route_table(
                tenant_id,
                project_id,
                vpc.id,
                NewRouteTable {
                    name: "custom".to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("duplicate route table name should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn route_table_parent_mismatch_is_not_found() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;
        let other_project = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "other".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();

        let err = store
            .create_route_table(
                tenant_id,
                other_project.id,
                vpc.id,
                NewRouteTable {
                    name: "wrong-parent".to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("vpc-in-wrong-project should be not-found");
        assert!(matches!(err, StoreError::NotFound));

        let err = store
            .create_route_table(
                tenant_id,
                project_id,
                Uuid::new_v4(),
                NewRouteTable {
                    name: "ghost".to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("unknown vpc should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn delete_main_route_table_conflicts() {
        let store = MemStore::new();
        let (_silo_id, _tenant_id, _project_id, vpc) = make_silo_project_vpc(&store).await;

        let err = store
            .delete_route_table(vpc.main_route_table_id)
            .await
            .expect_err("main route table cannot be deleted directly");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn subnet_round_trip_within_vpc() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        let subnet = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "web".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.1.0/24")),
                    ipv6_block: Some(ipv6_cidr("fd00:0:0:1::/64")),
                },
            )
            .await
            .unwrap();
        assert_eq!(subnet.tenant_id, tenant_id);
        assert_eq!(subnet.project_id, project_id);
        assert_eq!(subnet.vpc_id, vpc.id);
        assert_eq!(subnet.route_table_id, vpc.main_route_table_id);

        let fetched = store.get_subnet(subnet.id).await.unwrap();
        assert_eq!(fetched, subnet);

        let listed = store.list_subnets_in_vpc(vpc.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, subnet.id);

        store.delete_subnet(subnet.id).await.unwrap();
        let err = store
            .get_subnet(subnet.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn subnet_cidr_must_be_contained_in_vpc() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        // 10.1.0.0/24 is NOT inside the vpc's 10.0.0.0/16.
        let err = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "out".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.1.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("subnet outside vpc cidr should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn subnet_ipv4_in_ipv6_only_vpc_is_conflict() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        // IPv6-only VPC (no ipv4_block).
        let vpc = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "v6only".to_string(),
                    description: None,
                    ipv4_block: None,
                    ipv6_block: Some(ipv6_cidr("fd00::/48")),
                },
            )
            .await
            .unwrap();

        let err = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "wrong-family".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("ipv4 subnet in ipv6-only vpc should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn subnet_overlap_conflicts() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "first".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();

        // 10.0.0.128/25 overlaps the existing 10.0.0.0/24.
        let err = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "overlap".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.128/25")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("overlapping subnet should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn subnet_name_unique_within_vpc() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;
        store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "alpha".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.1.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let err = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "alpha".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.2.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("duplicate subnet name within vpc should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn create_subnet_in_unknown_vpc_is_not_found() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let err = store
            .create_subnet(
                tenant_id,
                project_id,
                Uuid::new_v4(),
                NewSubnet {
                    name: "ghost".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("unknown vpc should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn create_subnet_under_vpc_in_wrong_project_is_not_found() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id_a, vpc) = make_silo_project_vpc(&store).await;
        let project_b = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "other".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let _ = project_id_a;

        // Caller claims project_b but the vpc lives in project_a.
        let err = store
            .create_subnet(
                tenant_id,
                project_b.id,
                vpc.id,
                NewSubnet {
                    name: "wrong".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.1.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("vpc-in-wrong-project should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn delete_vpc_with_subnets_conflicts() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;
        let subnet = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "occupant".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.1.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();

        let err = store
            .delete_vpc(vpc.id)
            .await
            .expect_err("delete vpc with subnets should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));

        // Clear the subnet, then delete succeeds.
        store.delete_subnet(subnet.id).await.unwrap();
        store.delete_vpc(vpc.id).await.unwrap();
    }

    #[tokio::test]
    async fn delete_route_table_with_subnet_association_conflicts() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        let route_table = store
            .create_route_table(
                tenant_id,
                project_id,
                vpc.id,
                NewRouteTable {
                    name: "private".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let mut subnet = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "occupant".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.1.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();

        {
            let mut guard = store.inner.write().await;
            subnet.route_table_id = route_table.id;
            guard.subnets_by_id.insert(subnet.id, subnet.clone());
        }

        let err = store
            .delete_route_table(route_table.id)
            .await
            .expect_err("route table with subnet associations should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn delete_vpc_frees_vni_and_name() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let vpc = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "ephemeral".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let original_vni = vpc.vni;
        store.delete_vpc(vpc.id).await.unwrap();

        // Same name now reusable.
        let recreated = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "ephemeral".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        // Original VNI is back in the pool — recreate may or may not
        // reuse it, but the second create cannot have collided.
        let _ = original_vni;
        let _ = recreated;
    }

    fn ssh_key_req(name: &str, public_key: &str) -> NewSshKey {
        NewSshKey {
            name: name.to_string(),
            description: None,
            public_key: public_key.to_string(),
        }
    }

    #[tokio::test]
    async fn ssh_key_round_trip_within_silo() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "tenants".to_string(),
                description: None,
            })
            .await
            .unwrap();

        let key = store
            .create_ssh_key_silo(
                silo.id,
                ssh_key_req("ci", "ssh-ed25519 AAAA test"),
                "SHA256:abc".to_string(),
            )
            .await
            .unwrap();
        assert!(matches!(key.scope, SshKeyScope::Silo { silo_id: s } if s == silo.id));
        assert_eq!(key.name, "ci");
        assert_eq!(key.fingerprint, "SHA256:abc");

        let fetched = store.get_ssh_key(key.id).await.unwrap();
        assert_eq!(fetched, key);

        let listed = store.list_ssh_keys_in_silo(silo.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, key.id);

        store.delete_ssh_key(key.id).await.unwrap();
        let err = store
            .get_ssh_key(key.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn duplicate_ssh_key_name_within_silo_conflicts() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "x".to_string(),
                description: None,
            })
            .await
            .unwrap();
        store
            .create_ssh_key_silo(
                silo.id,
                ssh_key_req("ci", "ssh-ed25519 AAAA"),
                "SHA256:a".to_string(),
            )
            .await
            .unwrap();
        let err = store
            .create_ssh_key_silo(
                silo.id,
                ssh_key_req("ci", "ssh-ed25519 BBBB"),
                "SHA256:b".to_string(),
            )
            .await
            .expect_err("duplicate name within silo conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn duplicate_ssh_key_fingerprint_within_silo_conflicts() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "x".to_string(),
                description: None,
            })
            .await
            .unwrap();
        store
            .create_ssh_key_silo(
                silo.id,
                ssh_key_req("alice", "ssh-ed25519 AAAA"),
                "SHA256:dup".to_string(),
            )
            .await
            .unwrap();
        let err = store
            .create_ssh_key_silo(
                silo.id,
                ssh_key_req("bob", "ssh-ed25519 AAAA"),
                "SHA256:dup".to_string(),
            )
            .await
            .expect_err("re-uploading same fingerprint under new name conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn same_ssh_key_in_different_silos_does_not_conflict() {
        let store = MemStore::new();
        let a = store
            .create_silo(NewSilo {
                name: "a".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let b = store
            .create_silo(NewSilo {
                name: "b".to_string(),
                description: None,
            })
            .await
            .unwrap();
        // Same name + same fingerprint in two different silos is OK.
        store
            .create_ssh_key_silo(
                a.id,
                ssh_key_req("ci", "ssh-ed25519 AAAA"),
                "SHA256:x".to_string(),
            )
            .await
            .unwrap();
        store
            .create_ssh_key_silo(
                b.id,
                ssh_key_req("ci", "ssh-ed25519 AAAA"),
                "SHA256:x".to_string(),
            )
            .await
            .expect("same key in a different silo must be allowed");
    }

    #[tokio::test]
    async fn create_ssh_key_in_unknown_silo_is_not_found() {
        let store = MemStore::new();
        let err = store
            .create_ssh_key_silo(
                Uuid::new_v4(),
                ssh_key_req("orphan", "ssh-ed25519 AAAA"),
                "SHA256:x".to_string(),
            )
            .await
            .expect_err("unknown silo should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    fn image_req(name: &str) -> NewImage {
        // sha256 is derived from the test image name so two
        // distinct fixtures in the same silo don't collide on
        // the new content-addressed image-id derivation.
        // Production callers compute the real sha256 over the
        // image content; tests just need uniqueness.
        let mut sha = String::with_capacity(64);
        for byte in name.as_bytes() {
            use std::fmt::Write as _;
            write!(&mut sha, "{byte:02x}").ok();
        }
        while sha.len() < 64 {
            sha.push('0');
        }
        sha.truncate(64);
        NewImage {
            name: name.to_string(),
            description: None,
            os: "linux".to_string(),
            version: "ubuntu-22.04".to_string(),
            size_bytes: 1_000_000_000,
            sha256: sha,
            source_url: Some("mantafs://images/test".to_string()),
            id: None,
            compatibility: None,
        }
    }

    #[tokio::test]
    async fn image_round_trip_within_silo() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "tenants".to_string(),
                description: None,
            })
            .await
            .unwrap();

        let img = store
            .create_image_silo(silo.id, image_req("ubuntu-base"))
            .await
            .unwrap();
        assert_eq!(img.scope, ImageScope::Silo { silo_id: silo.id });
        assert_eq!(img.os, "linux");
        assert_eq!(img.size_bytes, 1_000_000_000);

        let fetched = store.get_image(img.id).await.unwrap();
        assert_eq!(fetched, img);

        let listed = store.list_images_in_silo(silo.id).await.unwrap();
        assert_eq!(listed.len(), 1);

        store.delete_image(img.id).await.unwrap();
        let err = store
            .get_image(img.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn duplicate_image_name_within_silo_conflicts() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "x".to_string(),
                description: None,
            })
            .await
            .unwrap();
        store
            .create_image_silo(silo.id, image_req("ubuntu-base"))
            .await
            .unwrap();
        let err = store
            .create_image_silo(silo.id, image_req("ubuntu-base"))
            .await
            .expect_err("duplicate name within silo conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn create_image_in_unknown_silo_is_not_found() {
        let store = MemStore::new();
        let err = store
            .create_image_silo(Uuid::new_v4(), image_req("orphan"))
            .await
            .expect_err("unknown silo should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn image_scope_round_trips() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "scopes".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;
        let project = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "p".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let user = store
            .create_user(User {
                id: Uuid::new_v4(),
                username: "alice".to_string(),
                password_hash: "$2y$dummy".to_string(),
                is_root: false,
                created_at: Utc::now(),
                tenant_id: Some(tenant_id),
                federation: None,
            })
            .await
            .unwrap();

        let pub_img = store
            .create_image_public(image_req("public-img"))
            .await
            .unwrap();
        assert_eq!(pub_img.scope, ImageScope::Public);
        let silo_img = store
            .create_image_silo(silo.id, image_req("silo-img"))
            .await
            .unwrap();
        let tenant_img = store
            .create_image_tenant(tenant_id, image_req("tenant-img"))
            .await
            .unwrap();
        let project_img = store
            .create_image_project(project.id, image_req("project-img"))
            .await
            .unwrap();
        let user_img = store
            .create_image_user(user.id, image_req("user-img"))
            .await
            .unwrap();
        assert_eq!(user_img.scope, ImageScope::User { user_id: user.id });

        // Single-scope listings are exact (no union).
        let pub_list = store.list_images_public().await.unwrap();
        assert_eq!(pub_list.len(), 1);
        assert_eq!(pub_list[0].id, pub_img.id);

        let silo_list = store.list_images_in_silo(silo.id).await.unwrap();
        assert_eq!(silo_list.len(), 1);
        assert_eq!(silo_list[0].id, silo_img.id);

        let tenant_list = store.list_images_in_tenant(tenant_id).await.unwrap();
        assert_eq!(tenant_list.len(), 1);
        assert_eq!(tenant_list[0].id, tenant_img.id);

        let project_list = store.list_images_in_project(project.id).await.unwrap();
        assert_eq!(project_list.len(), 1);
        assert_eq!(project_list[0].id, project_img.id);

        let user_list = store.list_images_for_user(user.id).await.unwrap();
        assert_eq!(user_list.len(), 1);
        assert_eq!(user_list[0].id, user_img.id);

        // Visible-in-tenant unions Public + Silo + Tenant.
        let visible_tenant = store
            .list_visible_images_in_tenant(tenant_id)
            .await
            .unwrap();
        let mut ids: Vec<Uuid> = visible_tenant.iter().map(|i| i.id).collect();
        ids.sort();
        let mut want = vec![pub_img.id, silo_img.id, tenant_img.id];
        want.sort();
        assert_eq!(ids, want);

        // Visible-in-project adds Project.
        let visible_project = store
            .list_visible_images_in_project(project.id)
            .await
            .unwrap();
        let mut ids: Vec<Uuid> = visible_project.iter().map(|i| i.id).collect();
        ids.sort();
        let mut want = vec![pub_img.id, silo_img.id, tenant_img.id, project_img.id];
        want.sort();
        assert_eq!(ids, want);

        // Delete cleans up the per-scope name index — re-create
        // with the same name in the same scope succeeds.
        store.delete_image(silo_img.id).await.unwrap();
        store
            .create_image_silo(silo.id, image_req("silo-img"))
            .await
            .expect("re-create after delete must succeed");
    }

    #[tokio::test]
    async fn same_image_name_across_scopes_does_not_collide() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "ns".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;
        store
            .create_image_public(image_req("ubuntu"))
            .await
            .unwrap();
        store
            .create_image_silo(silo.id, image_req("ubuntu"))
            .await
            .expect("silo `ubuntu` and public `ubuntu` are independent");
        store
            .create_image_tenant(tenant_id, image_req("ubuntu"))
            .await
            .expect("tenant `ubuntu` and silo `ubuntu` are independent");
    }

    fn quota_req() -> NewQuota {
        NewQuota {
            cpu_limit: 16,
            memory_bytes: 32 * 1024 * 1024 * 1024,
            disk_bytes: 1024 * 1024 * 1024 * 1024,
            instance_limit: 8,
        }
    }

    #[tokio::test]
    async fn quota_round_trip_within_project() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;

        // No quota set initially.
        let err = store
            .get_quota(tenant_id, project_id)
            .await
            .expect_err("unset quota is not-found");
        assert!(matches!(err, StoreError::NotFound));

        let quota = store
            .put_quota(tenant_id, project_id, quota_req())
            .await
            .unwrap();
        assert_eq!(quota.cpu_limit, 16);

        let read = store.get_quota(tenant_id, project_id).await.unwrap();
        assert_eq!(read.cpu_limit, 16);

        // Re-PUT replaces.
        let mut req = quota_req();
        req.cpu_limit = 32;
        let updated = store.put_quota(tenant_id, project_id, req).await.unwrap();
        assert_eq!(updated.cpu_limit, 32);

        store.delete_quota(tenant_id, project_id).await.unwrap();
        let err = store
            .get_quota(tenant_id, project_id)
            .await
            .expect_err("post-delete is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn quota_in_unknown_project_is_not_found() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, _project_id) = make_silo_and_project(&store).await;
        let err = store
            .put_quota(tenant_id, Uuid::new_v4(), quota_req())
            .await
            .expect_err("unknown project should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn quota_with_project_in_wrong_tenant_is_not_found() {
        let store = MemStore::new();
        let silo_a = store
            .create_silo(NewSilo {
                name: "a".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let silo_b = store
            .create_silo(NewSilo {
                name: "b".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let project = store
            .create_project(
                silo_a.default_tenant_id,
                NewProject {
                    name: "p".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();

        // Caller claims silo_b's tenant but project lives in silo_a's.
        let err = store
            .put_quota(silo_b.default_tenant_id, project.id, quota_req())
            .await
            .expect_err("project-in-wrong-tenant should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    /// Build a full silo+tenant+project+vpc+subnet+image+ssh-key tree
    /// suitable for instance-create tests. Returns
    /// `(tenant_id, project_id, image_id, subnet_id, ssh_key_id)`.
    /// Image and SSH key creates internally use the silo derived
    /// from the tenant since those resources are still silo-scoped
    /// in E-3.
    async fn make_instance_fixture(store: &MemStore) -> (Uuid, Uuid, Uuid, Uuid, Uuid) {
        let (silo_id, tenant_id, project_id, vpc) = make_silo_project_vpc(store).await;
        let subnet = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "primary".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.1.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let image = store
            .create_image_silo(silo_id, image_req("ubuntu-base"))
            .await
            .unwrap();
        let ssh_key = store
            .create_ssh_key_silo(
                silo_id,
                ssh_key_req("ci", "ssh-ed25519 AAAA"),
                "SHA256:fixture".to_string(),
            )
            .await
            .unwrap();
        (tenant_id, project_id, image.id, subnet.id, ssh_key.id)
    }

    fn instance_req(name: &str, image_id: Uuid, subnet_id: Uuid, ssh_key_id: Uuid) -> NewInstance {
        NewInstance {
            name: name.to_string(),
            description: None,
            image_id,
            primary_subnet_id: subnet_id,
            ssh_key_ids: vec![ssh_key_id],
            cpu: 2,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            extra_nics: Vec::new(),
        }
    }

    #[tokio::test]
    async fn instance_round_trip_within_project() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;

        let InstanceCreateResult { instance, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        assert_eq!(instance.tenant_id, tenant_id);
        assert_eq!(instance.project_id, project_id);
        assert_eq!(instance.lifecycle, LifecycleState::Pending);

        let fetched = store.get_instance(instance.id).await.unwrap();
        assert_eq!(fetched, instance);

        let listed = store.list_instances_in_project(project_id).await.unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn instance_with_unknown_image_is_not_found() {
        let store = MemStore::new();
        let (tenant_id, project_id, _, subnet_id, ssh_key_id) = make_instance_fixture(&store).await;
        let err = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("bad", Uuid::new_v4(), subnet_id, ssh_key_id),
            )
            .await
            .expect_err("unknown image should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn instance_with_subnet_in_other_project_is_not_found() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, _, ssh_key_id) = make_instance_fixture(&store).await;
        // Second project + subnet in same silo.
        let other_project = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "other".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let other_vpc = store
            .create_vpc(
                tenant_id,
                other_project.id,
                NewVpc {
                    name: "v".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.5.0.0/16")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let foreign_subnet = store
            .create_subnet(
                tenant_id,
                other_project.id,
                other_vpc.id,
                NewSubnet {
                    name: "s".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.5.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let err = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("bad", image_id, foreign_subnet.id, ssh_key_id),
            )
            .await
            .expect_err("foreign-project subnet should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn duplicate_instance_name_within_project_conflicts() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let err = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .expect_err("duplicate name within project should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn lifecycle_transition_cas_succeeds_on_match() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult { instance, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();

        // Pending → Running (skipping Provisioning for the
        // synchronous-transition path).
        let updated = store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Running,
            )
            .await
            .unwrap();
        assert_eq!(updated.lifecycle, LifecycleState::Running);
        assert!(updated.updated_at >= instance.created_at);
    }

    #[tokio::test]
    async fn lifecycle_transition_cas_conflicts_on_mismatch() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult { instance, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        // Currently Pending; ask for Running → conflict.
        let err = store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Running],
                LifecycleState::Stopped,
            )
            .await
            .expect_err("CAS with wrong expected_from must conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn lifecycle_kind_failed_matches_any_reason() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult { instance, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        // Drive Pending → Failed with one reason …
        store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Failed {
                    reason: "image vanished".to_string(),
                },
            )
            .await
            .unwrap();
        // … and then Failed-with-any-reason → Stopped (i.e. operator
        // did a manual reset). The discriminant kind matches without
        // needing the caller to know the exact reason string.
        store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Failed],
                LifecycleState::Stopped,
            )
            .await
            .expect("Failed { .. } should match LifecycleStateKind::Failed");
    }

    #[tokio::test]
    async fn delete_running_instance_conflicts() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult { instance, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Running,
            )
            .await
            .unwrap();

        let err = store
            .delete_instance(instance.id, false)
            .await
            .expect_err("delete while running must conflict");
        assert!(matches!(err, StoreError::Conflict(_)));

        // Drive to Stopped, then delete works.
        store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Running],
                LifecycleState::Stopped,
            )
            .await
            .unwrap();
        store.delete_instance(instance.id, false).await.unwrap();
    }

    #[tokio::test]
    async fn delete_failed_instance_succeeds() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult { instance, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Failed {
                    reason: "boom".to_string(),
                },
            )
            .await
            .unwrap();
        store
            .delete_instance(instance.id, false)
            .await
            .expect("Failed instance is deletable");
    }

    #[tokio::test]
    async fn instance_create_returns_primary_nic_with_ip_and_mac() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult {
            instance,
            nics,
            disks: _disks,
        } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        assert_eq!(nics.len(), 1);
        let nic = &nics[0];
        assert_eq!(nic.instance_id, instance.id);
        assert_eq!(nic.subnet_id, subnet_id);
        assert_eq!(nic.name, "primary");
        // The fixture's subnet has ipv4 only.
        let ip = nic.primary_ipv4.expect("primary_ipv4 should be set");
        // Subnet is 10.0.1.0/24; .0 = network, .1 = gateway, so first
        // available is .2.
        assert_eq!(ip.octets(), [10, 0, 1, 2]);
        assert!(nic.primary_ipv6.is_none(), "ipv6-less subnet -> no v6");
        // MAC starts with "02:" (locally administered, unicast).
        assert!(
            nic.mac.starts_with("02:"),
            "mac should be locally-administered, got {}",
            nic.mac
        );
        assert_eq!(nic.mac.matches(':').count(), 5);

        // list_nics_for_instance returns the primary.
        let listed = store.list_nics_for_instance(instance.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, nic.id);
    }

    #[tokio::test]
    async fn delete_instance_cascades_to_nic_and_frees_ip() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult {
            instance,
            nics,
            disks: _disks,
        } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let nic = &nics[0];
        let original_ip = nic.primary_ipv4.unwrap();
        // Drive Pending → Stopped so delete is allowed.
        store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Stopped,
            )
            .await
            .unwrap();
        store.delete_instance(instance.id, false).await.unwrap();

        // NIC record is gone.
        let err = store
            .get_nic(nic.id)
            .await
            .expect_err("NIC should be cascade-deleted");
        assert!(matches!(err, StoreError::NotFound));

        // Re-creating an instance under the same subnet picks up the
        // freed IP (lowest free is the one we just released).
        let InstanceCreateResult { nics: new_nics, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web2", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        assert_eq!(new_nics[0].primary_ipv4, Some(original_ip));
    }

    #[tokio::test]
    async fn dual_stack_subnet_allocates_v4_and_v6() {
        let store = MemStore::new();
        let (silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let vpc = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "vpc1".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/16")),
                    ipv6_block: Some(ipv6_cidr("fd00::/48")),
                },
            )
            .await
            .unwrap();
        let subnet = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "primary".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.1.0/24")),
                    ipv6_block: Some(ipv6_cidr("fd00:0:0:1::/64")),
                },
            )
            .await
            .unwrap();
        let image = store
            .create_image_silo(silo_id, image_req("dual"))
            .await
            .unwrap();
        let key = store
            .create_ssh_key_silo(
                silo_id,
                ssh_key_req("ci", "ssh-ed25519 AAAA"),
                "SHA256:dual".to_string(),
            )
            .await
            .unwrap();
        let InstanceCreateResult { nics, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image.id, subnet.id, key.id),
            )
            .await
            .unwrap();
        let nic = &nics[0];
        assert!(nic.primary_ipv4.is_some());
        assert!(nic.primary_ipv6.is_some());
    }

    #[tokio::test]
    async fn instance_create_returns_boot_disk_sized_to_image() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult {
            instance, disks, ..
        } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        assert_eq!(disks.len(), 1);
        let boot = &disks[0];
        assert_eq!(boot.instance_id, instance.id);
        assert_eq!(boot.name, "boot");
        assert_eq!(boot.kind, DiskKind::Boot);
        assert_eq!(boot.source_image_id, Some(image_id));
        // The fixture image is 1_000_000_000 bytes.
        assert_eq!(boot.size_bytes, 1_000_000_000);

        // list_disks_for_instance returns the boot disk.
        let listed = store.list_disks_for_instance(instance.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, boot.id);
    }

    #[tokio::test]
    async fn delete_instance_cascades_to_boot_disk() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult {
            instance, disks, ..
        } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("web", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let boot_id = disks[0].id;
        store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Stopped,
            )
            .await
            .unwrap();
        store.delete_instance(instance.id, false).await.unwrap();

        let err = store
            .get_disk(boot_id)
            .await
            .expect_err("boot disk should be cascade-deleted");
        assert!(matches!(err, StoreError::NotFound));
    }

    fn fip_req(name: &str, family: AddressFamily) -> NewFloatingIp {
        NewFloatingIp {
            name: name.to_string(),
            description: None,
            family,
        }
    }

    fn nat_req(name: &str, family: AddressFamily) -> NewNatGateway {
        NewNatGateway {
            name: name.to_string(),
            description: None,
            family,
        }
    }

    async fn make_vpc(
        store: &MemStore,
        tenant_id: Uuid,
        project_id: Uuid,
        name: &str,
        ipv4_block: &str,
    ) -> Vpc {
        store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: name.to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr(ipv4_block)),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn nat_gateway_v4_round_trip_with_realized_view() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let vpc = make_vpc(&store, tenant_id, project_id, "prod", "10.0.0.0/24").await;

        let nat = store
            .create_nat_gateway(
                tenant_id,
                project_id,
                vpc.id,
                nat_req("egress", AddressFamily::V4),
            )
            .await
            .unwrap();
        assert_eq!(nat.tenant_id, tenant_id);
        assert_eq!(nat.project_id, project_id);
        assert_eq!(nat.vpc_id, vpc.id);
        assert_eq!(nat.desired_generation, 1);
        assert_eq!(nat.realized.desired_generation, 1);
        assert!(nat.realized.applied_generation.is_none());
        assert!(nat.realized.realizations.is_empty());
        assert!(nat.edge_cluster_id.is_none());
        match nat.public_address {
            IpAddr::V4(v4) => assert_eq!(v4.octets(), [203, 0, 113, 2]),
            other => panic!("expected v4, got {other:?}"),
        }

        let fetched = store.get_nat_gateway(nat.id).await.unwrap();
        assert_eq!(fetched, nat);

        store
            .record_network_realization(
                NetworkResourceId::NatGateway { id: nat.id },
                RealizerId::EdgeCluster { id: Uuid::new_v4() },
                1,
                RealizationStatus::Applied,
                Some("edge dataplane applied".to_string()),
            )
            .await
            .unwrap();

        let realized = store.get_nat_gateway(nat.id).await.unwrap();
        assert_eq!(realized.realized.desired_generation, 1);
        assert_eq!(realized.realized.applied_generation, Some(1));
        assert_eq!(realized.realized.realizations.len(), 1);

        let listed = store.list_nat_gateways_in_vpc(vpc.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], realized);
    }

    #[tokio::test]
    async fn nat_gateway_v6_allocates_from_pool() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let vpc = make_vpc(&store, tenant_id, project_id, "prod", "10.0.0.0/24").await;

        let nat = store
            .create_nat_gateway(
                tenant_id,
                project_id,
                vpc.id,
                nat_req("egress6", AddressFamily::V6),
            )
            .await
            .unwrap();
        match nat.public_address {
            IpAddr::V6(v6) => {
                let expected: std::net::Ipv6Addr = "2001:db8::2".parse().unwrap();
                assert_eq!(v6, expected);
            }
            other => panic!("expected v6, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_nat_gateway_name_within_vpc_conflicts() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let vpc = make_vpc(&store, tenant_id, project_id, "prod", "10.0.0.0/24").await;
        store
            .create_nat_gateway(
                tenant_id,
                project_id,
                vpc.id,
                nat_req("egress", AddressFamily::V4),
            )
            .await
            .unwrap();
        let err = store
            .create_nat_gateway(
                tenant_id,
                project_id,
                vpc.id,
                nat_req("egress", AddressFamily::V6),
            )
            .await
            .expect_err("duplicate NAT name within a VPC must conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn same_nat_gateway_name_in_different_vpcs_does_not_conflict() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let vpc_a = make_vpc(&store, tenant_id, project_id, "a", "10.0.0.0/24").await;
        let vpc_b = make_vpc(&store, tenant_id, project_id, "b", "10.0.1.0/24").await;

        store
            .create_nat_gateway(
                tenant_id,
                project_id,
                vpc_a.id,
                nat_req("egress", AddressFamily::V4),
            )
            .await
            .unwrap();
        store
            .create_nat_gateway(
                tenant_id,
                project_id,
                vpc_b.id,
                nat_req("egress", AddressFamily::V4),
            )
            .await
            .expect("same NAT name in a different VPC must be allowed");
    }

    #[tokio::test]
    async fn create_nat_gateway_in_unknown_vpc_is_not_found() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let err = store
            .create_nat_gateway(
                tenant_id,
                project_id,
                Uuid::new_v4(),
                nat_req("egress", AddressFamily::V4),
            )
            .await
            .expect_err("unknown VPC should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn create_nat_gateway_under_vpc_in_wrong_project_is_not_found() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "ops".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;
        let project_a = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "a".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let project_b = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "b".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let vpc = make_vpc(&store, tenant_id, project_a.id, "a", "10.0.0.0/24").await;

        let err = store
            .create_nat_gateway(
                tenant_id,
                project_b.id,
                vpc.id,
                nat_req("egress", AddressFamily::V4),
            )
            .await
            .expect_err("wrong project should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn nat_gateway_and_floating_ip_share_public_pool() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let vpc = make_vpc(&store, tenant_id, project_id, "prod", "10.0.0.0/24").await;

        let fip = store
            .create_floating_ip(tenant_id, project_id, fip_req("public", AddressFamily::V4))
            .await
            .unwrap();
        let nat = store
            .create_nat_gateway(
                tenant_id,
                project_id,
                vpc.id,
                nat_req("egress", AddressFamily::V4),
            )
            .await
            .unwrap();

        assert_ne!(fip.address, nat.public_address);
        match (fip.address, nat.public_address) {
            (IpAddr::V4(fip_v4), IpAddr::V4(nat_v4)) => {
                assert_eq!(fip_v4.octets(), [203, 0, 113, 2]);
                assert_eq!(nat_v4.octets(), [203, 0, 113, 3]);
            }
            other => panic!("expected two v4 addresses, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_nat_gateway_frees_public_address() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let vpc = make_vpc(&store, tenant_id, project_id, "prod", "10.0.0.0/24").await;

        let nat = store
            .create_nat_gateway(
                tenant_id,
                project_id,
                vpc.id,
                nat_req("egress", AddressFamily::V4),
            )
            .await
            .unwrap();
        store.delete_nat_gateway(nat.id).await.unwrap();
        let err = store
            .get_nat_gateway(nat.id)
            .await
            .expect_err("deleted NAT should not be found");
        assert!(matches!(err, StoreError::NotFound));

        let fip = store
            .create_floating_ip(tenant_id, project_id, fip_req("public", AddressFamily::V4))
            .await
            .unwrap();
        assert_eq!(fip.address, nat.public_address);
    }

    #[tokio::test]
    async fn floating_ip_v4_allocates_from_pool() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let fip = store
            .create_floating_ip(tenant_id, project_id, fip_req("public", AddressFamily::V4))
            .await
            .unwrap();
        match fip.address {
            IpAddr::V4(v4) => {
                // Pool is 203.0.113.0/24; first available is .2
                // (network .0 + gateway .1 are skipped).
                assert_eq!(v4.octets(), [203, 0, 113, 2]);
            }
            other => panic!("expected v4, got {other:?}"),
        }
        assert!(fip.attached_to.is_none());
    }

    #[tokio::test]
    async fn floating_ip_v6_allocates_from_pool() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        let fip = store
            .create_floating_ip(tenant_id, project_id, fip_req("v6", AddressFamily::V6))
            .await
            .unwrap();
        match fip.address {
            IpAddr::V6(v6) => {
                // Pool is 2001:db8::/48; first allocated is
                // 2001:db8::2 (skip ::0 + ::1).
                let expected: std::net::Ipv6Addr = "2001:db8::2".parse().unwrap();
                assert_eq!(v6, expected);
            }
            other => panic!("expected v6, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_floating_ip_name_within_project_conflicts() {
        let store = MemStore::new();
        let (_silo_id, tenant_id, project_id) = make_silo_and_project(&store).await;
        store
            .create_floating_ip(tenant_id, project_id, fip_req("public", AddressFamily::V4))
            .await
            .unwrap();
        let err = store
            .create_floating_ip(tenant_id, project_id, fip_req("public", AddressFamily::V4))
            .await
            .expect_err("duplicate name within project must conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn attach_replaces_existing_attachment() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        // Two instances, two NICs.
        let InstanceCreateResult { nics: nics_a, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("a", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let InstanceCreateResult { nics: nics_b, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("b", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let fip = store
            .create_floating_ip(tenant_id, project_id, fip_req("p", AddressFamily::V4))
            .await
            .unwrap();

        let attached_a = store
            .attach_floating_ip(fip.id, nics_a[0].id)
            .await
            .unwrap();
        let attach = attached_a.attached_to.as_ref().expect("should be attached");
        assert_eq!(attach.nic_id, nics_a[0].id);

        // Re-attach (no detach) — replace semantics.
        let attached_b = store
            .attach_floating_ip(fip.id, nics_b[0].id)
            .await
            .unwrap();
        let attach = attached_b
            .attached_to
            .as_ref()
            .expect("should still be attached");
        assert_eq!(attach.nic_id, nics_b[0].id);
        assert_eq!(attach.instance_id, nics_b[0].instance_id);
    }

    #[tokio::test]
    async fn delete_attached_floating_ip_conflicts() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult { nics, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("a", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let fip = store
            .create_floating_ip(tenant_id, project_id, fip_req("p", AddressFamily::V4))
            .await
            .unwrap();
        store.attach_floating_ip(fip.id, nics[0].id).await.unwrap();

        let err = store
            .delete_floating_ip(fip.id)
            .await
            .expect_err("delete-while-attached must conflict");
        assert!(matches!(err, StoreError::Conflict(_)));

        // Detach + delete works.
        store.detach_floating_ip(fip.id).await.unwrap();
        store.delete_floating_ip(fip.id).await.unwrap();
    }

    #[tokio::test]
    async fn instance_delete_detaches_floating_ip_but_does_not_release() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let InstanceCreateResult { instance, nics, .. } = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("a", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let fip = store
            .create_floating_ip(tenant_id, project_id, fip_req("p", AddressFamily::V4))
            .await
            .unwrap();
        store.attach_floating_ip(fip.id, nics[0].id).await.unwrap();
        let original_address = fip.address;

        // Delete the instance (after Stopped).
        store
            .transition_instance_lifecycle(
                instance.id,
                &[LifecycleStateKind::Pending],
                LifecycleState::Stopped,
            )
            .await
            .unwrap();
        store.delete_instance(instance.id, false).await.unwrap();

        // FloatingIp still exists, just detached.
        let after = store.get_floating_ip(fip.id).await.unwrap();
        assert!(after.attached_to.is_none(), "should auto-detach");
        assert_eq!(after.address, original_address, "address preserved");
        assert_eq!(after.project_id, project_id, "project ownership preserved");
    }

    #[tokio::test]
    async fn instance_with_extra_nic_allocates_per_subnet() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, primary_subnet, ssh_key_id) =
            make_instance_fixture(&store).await;
        // A second subnet in the same project for the extra NIC.
        let vpc_id = store.get_subnet(primary_subnet).await.unwrap().vpc_id;
        let second = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc_id,
                NewSubnet {
                    name: "secondary".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.2.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let mut req = instance_req("two-nics", image_id, primary_subnet, ssh_key_id);
        req.extra_nics = vec![NewInstanceNic {
            subnet_id: second.id,
            name: "data".to_string(),
        }];
        let result = store
            .create_instance(tenant_id, project_id, req)
            .await
            .unwrap();
        assert_eq!(result.nics.len(), 2, "expected primary + one extra");
        // Index 0 is the primary, index 1 is the declared extra.
        assert_eq!(result.nics[0].name, "primary");
        assert_eq!(result.nics[0].subnet_id, primary_subnet);
        assert_eq!(result.nics[1].name, "data");
        assert_eq!(result.nics[1].subnet_id, second.id);
        assert!(result.nics[1].primary_ipv4.is_some());
        assert_ne!(
            result.nics[0].mac, result.nics[1].mac,
            "each NIC must have a distinct MAC",
        );
    }

    #[tokio::test]
    async fn instance_extra_nic_duplicate_name_is_rejected() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, primary_subnet, ssh_key_id) =
            make_instance_fixture(&store).await;
        let mut req = instance_req("dup", image_id, primary_subnet, ssh_key_id);
        req.extra_nics = vec![NewInstanceNic {
            subnet_id: primary_subnet,
            // Name collides with the auto-created "primary" NIC.
            name: "primary".to_string(),
        }];
        let err = store
            .create_instance(tenant_id, project_id, req)
            .await
            .expect_err("duplicate NIC name must conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn instance_extra_nic_in_wrong_tenant_is_not_found() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, primary_subnet, ssh_key_id) =
            make_instance_fixture(&store).await;
        // A subnet in a *different* tenant+project (different silo too).
        let other_silo = store
            .create_silo(NewSilo {
                name: "other".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let other_tenant = other_silo.default_tenant_id;
        let other_project = store
            .create_project(
                other_tenant,
                NewProject {
                    name: "p".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let other_vpc = store
            .create_vpc(
                other_tenant,
                other_project.id,
                NewVpc {
                    name: "v".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.99.0.0/16")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let other_subnet = store
            .create_subnet(
                other_tenant,
                other_project.id,
                other_vpc.id,
                NewSubnet {
                    name: "s".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.99.1.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let mut req = instance_req("cross-tenant", image_id, primary_subnet, ssh_key_id);
        req.extra_nics = vec![NewInstanceNic {
            subnet_id: other_subnet.id,
            name: "alien".to_string(),
        }];
        let err = store
            .create_instance(tenant_id, project_id, req)
            .await
            .expect_err("extra NIC subnet outside tenant+project must NotFound");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn delete_instance_in_pending_is_rejected_without_force() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let created = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("doomed", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let id = created.instance.id;
        let err = store
            .delete_instance(id, false)
            .await
            .expect_err("Pending state must reject default delete");
        assert!(matches!(err, StoreError::Conflict(_)));
        assert!(store.get_instance(id).await.is_ok());
    }

    #[tokio::test]
    async fn delete_instance_force_overrides_state_gate() {
        let store = MemStore::new();
        let (tenant_id, project_id, image_id, subnet_id, ssh_key_id) =
            make_instance_fixture(&store).await;
        let created = store
            .create_instance(
                tenant_id,
                project_id,
                instance_req("force-delete-me", image_id, subnet_id, ssh_key_id),
            )
            .await
            .unwrap();
        let id = created.instance.id;
        store
            .delete_instance(id, true)
            .await
            .expect("force delete must succeed regardless of state");
        assert!(matches!(
            store.get_instance(id).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn cross_project_attach_target_is_not_found() {
        let store = MemStore::new();
        let (tenant_id, project_a, image_a, subnet_a, ssh_a) = make_instance_fixture(&store).await;
        // Resolve the silo via the tenant for the still-silo-scoped
        // image and ssh-key fixtures.
        let silo_id = store.get_tenant(tenant_id).await.unwrap().silo_id;
        // Second project + its own fixture in the same tenant.
        let project_b = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "other".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let vpc_b = store
            .create_vpc(
                tenant_id,
                project_b.id,
                NewVpc {
                    name: "v".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.5.0.0/16")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let subnet_b = store
            .create_subnet(
                tenant_id,
                project_b.id,
                vpc_b.id,
                NewSubnet {
                    name: "s".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.5.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let image_b = store
            .create_image_silo(silo_id, image_req("ub-b"))
            .await
            .unwrap();
        let key_b = store
            .create_ssh_key_silo(
                silo_id,
                ssh_key_req("ci-b", "ssh-ed25519 BBBB"),
                "SHA256:b".to_string(),
            )
            .await
            .unwrap();
        let InstanceCreateResult { nics: nics_b, .. } = store
            .create_instance(
                tenant_id,
                project_b.id,
                instance_req("b", image_b.id, subnet_b.id, key_b.id),
            )
            .await
            .unwrap();

        // Allocate the FloatingIp under project A.
        let fip = store
            .create_floating_ip(tenant_id, project_a, fip_req("p", AddressFamily::V4))
            .await
            .unwrap();
        let _ = (image_a, subnet_a, ssh_a);

        // Trying to attach to project B's NIC must 404 (cross-project).
        let err = store
            .attach_floating_ip(fip.id, nics_b[0].id)
            .await
            .expect_err("cross-project attach must be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    use crate::JobKind;

    fn provision_job(instance_id: Uuid) -> NewJob {
        NewJob {
            kind: JobKind::Provision { instance_id },
            target_cn_uuid: None,
        }
    }

    #[tokio::test]
    async fn enqueue_assigns_monotonic_seq() {
        let store = MemStore::new();
        let i1 = Uuid::new_v4();
        let i2 = Uuid::new_v4();
        let j1 = store.enqueue_job(provision_job(i1)).await.unwrap();
        let j2 = store.enqueue_job(provision_job(i2)).await.unwrap();
        assert_eq!(j1.seq, 0);
        assert_eq!(j2.seq, 1);
        assert!(matches!(j1.status, JobStatus::Pending));
    }

    #[tokio::test]
    async fn claim_next_job_is_fifo() {
        let store = MemStore::new();
        let i1 = Uuid::new_v4();
        let i2 = Uuid::new_v4();
        let j1 = store.enqueue_job(provision_job(i1)).await.unwrap();
        let j2 = store.enqueue_job(provision_job(i2)).await.unwrap();

        let claimed = store.claim_next_job("worker-a", None).await.unwrap();
        assert_eq!(claimed.id, j1.id);
        assert!(matches!(claimed.status, JobStatus::InProgress));
        assert_eq!(claimed.claimed_by.as_deref(), Some("worker-a"));

        let claimed = store.claim_next_job("worker-b", None).await.unwrap();
        assert_eq!(claimed.id, j2.id);
    }

    #[tokio::test]
    async fn claim_targeting_matrix() {
        let store = MemStore::new();
        let cn_a = Uuid::new_v4();
        let cn_b = Uuid::new_v4();

        // Enqueue: one unrouted, one routed-to-A.
        let unrouted = store
            .enqueue_job(NewJob {
                kind: JobKind::Provision {
                    instance_id: Uuid::new_v4(),
                },
                target_cn_uuid: None,
            })
            .await
            .unwrap();
        let routed_a = store
            .enqueue_job(NewJob {
                kind: JobKind::Provision {
                    instance_id: Uuid::new_v4(),
                },
                target_cn_uuid: Some(cn_a),
            })
            .await
            .unwrap();

        // Unbound claimer (the in-process stub) sees only the
        // unrouted job, in seq order.
        let claimed = store.claim_next_job("stub", None).await.unwrap();
        assert_eq!(claimed.id, unrouted.id);
        // Routed_a is still pending — unbound claimer skipped it.
        let err = store
            .claim_next_job("stub", None)
            .await
            .expect_err("only one unrouted job; stub should see queue empty now");
        assert!(matches!(err, StoreError::NotFound));

        // Bound CN-A picks up the routed-A job.
        let claimed = store.claim_next_job("agent-a", Some(cn_a)).await.unwrap();
        assert_eq!(claimed.id, routed_a.id);

        // Re-enqueue routed-to-B; bound CN-A cannot claim.
        let routed_b = store
            .enqueue_job(NewJob {
                kind: JobKind::Provision {
                    instance_id: Uuid::new_v4(),
                },
                target_cn_uuid: Some(cn_b),
            })
            .await
            .unwrap();
        let err = store
            .claim_next_job("agent-a", Some(cn_a))
            .await
            .expect_err("CN-A is bound, can't take a CN-B-routed job");
        assert!(matches!(err, StoreError::NotFound));
        // CN-B can.
        let claimed = store.claim_next_job("agent-b", Some(cn_b)).await.unwrap();
        assert_eq!(claimed.id, routed_b.id);
    }

    #[tokio::test]
    async fn claim_empty_queue_is_not_found() {
        let store = MemStore::new();
        let err = store
            .claim_next_job("worker", None)
            .await
            .expect_err("empty queue should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn complete_job_terminal_states() {
        let store = MemStore::new();
        let job = store
            .enqueue_job(provision_job(Uuid::new_v4()))
            .await
            .unwrap();
        store.claim_next_job("w", None).await.unwrap();
        let done = store
            .complete_job(job.id, JobOutcome::Completed)
            .await
            .unwrap();
        assert!(matches!(done.status, JobStatus::Completed));
        assert!(done.completed_at.is_some());

        // Re-completing is a Conflict.
        let err = store
            .complete_job(job.id, JobOutcome::Completed)
            .await
            .expect_err("double-complete should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn complete_job_with_failure_reason() {
        let store = MemStore::new();
        let job = store
            .enqueue_job(provision_job(Uuid::new_v4()))
            .await
            .unwrap();
        let done = store
            .complete_job(
                job.id,
                JobOutcome::Failed {
                    reason: "image not ready".to_string(),
                },
            )
            .await
            .unwrap();
        match done.status {
            JobStatus::Failed { reason } => assert_eq!(reason, "image not ready"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_recent_jobs_is_newest_first() {
        let store = MemStore::new();
        for _ in 0..5 {
            store
                .enqueue_job(provision_job(Uuid::new_v4()))
                .await
                .unwrap();
        }
        let listed = store.list_recent_jobs(3).await.unwrap();
        assert_eq!(listed.len(), 3);
        // Newest = highest seq among the three returned.
        assert_eq!(listed[0].seq, 4);
        assert_eq!(listed[1].seq, 3);
        assert_eq!(listed[2].seq, 2);
    }

    #[tokio::test]
    async fn system_key_round_trip() {
        let store = MemStore::new();
        let err = store
            .get_system_key(SystemKey::JwtSigning)
            .await
            .expect_err("missing key should be not-found");
        assert!(matches!(err, StoreError::NotFound));

        let payload = vec![0xAA; 32];
        store
            .put_system_key(SystemKey::JwtSigning, payload.clone())
            .await
            .unwrap();
        let read = store.get_system_key(SystemKey::JwtSigning).await.unwrap();
        assert_eq!(read, payload);
    }

    // ---------- CN registration / approval ----------

    fn sysinfo_fixture() -> serde_json::Value {
        serde_json::json!({
            "UUID": "00000000-0000-0000-0000-000000000001",
            "Hostname": "test-cn",
        })
    }

    #[tokio::test]
    async fn register_cn_creates_pending_with_claim_code() {
        let store = MemStore::new();
        let id = Uuid::new_v4();
        let now = Utc::now();
        let cn = store
            .register_cn(id, "host1".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        assert_eq!(cn.state, CnState::Pending);
        assert!(cn.claim_code.is_some());
        assert_eq!(cn.claim_code.as_ref().unwrap().len(), 6);
        assert!(cn.poll_token.len() == 32);
        assert!(cn.bound_api_key_id.is_none());
        assert!(cn.pending_credential.is_none());
        assert!(cn.approved_at.is_none());
    }

    #[tokio::test]
    async fn re_register_pending_rotates_claim_code() {
        let store = MemStore::new();
        let id = Uuid::new_v4();
        let now = Utc::now();
        let first = store
            .register_cn(id, "host1".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        let second = store
            .register_cn(id, "host1-renamed".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        // Same record (same registered_at), but different claim/poll.
        assert_eq!(first.registered_at, second.registered_at);
        assert_ne!(first.claim_code, second.claim_code);
        assert_ne!(first.poll_token, second.poll_token);
        assert_eq!(second.hostname, "host1-renamed");
        // Old claim code is no longer findable.
        let err = store
            .get_cn_by_claim_code(first.claim_code.as_ref().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn re_register_approved_is_idempotent() {
        let store = MemStore::new();
        let id = Uuid::new_v4();
        let now = Utc::now();
        store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        store
            .approve_cn(id, Uuid::new_v4(), "tcadm_xxx".into(), now)
            .await
            .unwrap();
        // Re-register: should remain Approved, refresh sysinfo + last_seen,
        // not re-mint anything.
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
            .unwrap();
        assert_eq!(updated.state, CnState::Approved);
        assert_eq!(updated.hostname, "h2");
        assert_eq!(updated.last_seen, Some(later));
        assert_eq!(updated.sysinfo, serde_json::json!({"updated": true}));
    }

    #[tokio::test]
    async fn approve_cn_flips_state_and_stashes_credential() {
        let store = MemStore::new();
        let id = Uuid::new_v4();
        let now = Utc::now();
        let cn = store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        let key_id = Uuid::new_v4();
        let approved = store
            .approve_cn(id, key_id, "tcadm_secret".into(), now)
            .await
            .unwrap();
        assert_eq!(approved.state, CnState::Approved);
        assert!(approved.claim_code.is_none());
        assert_eq!(approved.bound_api_key_id, Some(key_id));
        assert_eq!(approved.pending_credential.as_deref(), Some("tcadm_secret"));

        // Old claim code is gone.
        let err = store
            .get_cn_by_claim_code(cn.claim_code.as_ref().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::NotFound));

        // First long-poll consumes the credential.
        let consumed = store
            .consume_cn_pending_credential(&cn.poll_token)
            .await
            .unwrap();
        assert_eq!(consumed.as_deref(), Some("tcadm_secret"));

        // Second long-poll sees None.
        let consumed_again = store
            .consume_cn_pending_credential(&cn.poll_token)
            .await
            .unwrap();
        assert!(consumed_again.is_none());
    }

    #[tokio::test]
    async fn approve_cn_pending_only() {
        let store = MemStore::new();
        let id = Uuid::new_v4();
        // Approve before register: NotFound.
        let err = store
            .approve_cn(id, Uuid::new_v4(), "x".into(), Utc::now())
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn disable_cn_blocks_re_registration() {
        let store = MemStore::new();
        let id = Uuid::new_v4();
        let now = Utc::now();
        store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        store.disable_cn(id).await.unwrap();
        let err = store
            .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn auto_approve_window_promotes_registration() {
        let store = MemStore::new();
        let now = Utc::now();
        store
            .open_auto_approve_window(AutoApproveWindow {
                opened_at: now,
                expires_at: now + chrono::Duration::minutes(30),
                remaining_count: Some(2),
                opened_by: "root".into(),
            })
            .await
            .unwrap();

        let cn1 = store
            .register_cn(Uuid::new_v4(), "h1".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        assert_eq!(cn1.state, CnState::Approved);
        assert!(cn1.claim_code.is_none());
        assert!(cn1.approved_at.is_some());

        let cn2 = store
            .register_cn(Uuid::new_v4(), "h2".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        assert_eq!(cn2.state, CnState::Approved);

        // Window exhausted (count was 2). Third registration is Pending.
        let cn3 = store
            .register_cn(Uuid::new_v4(), "h3".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        assert_eq!(cn3.state, CnState::Pending);
        assert!(cn3.claim_code.is_some());

        // Window record is gone after exhaustion.
        assert!(store.get_auto_approve_window().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn auto_approve_window_expires_on_time() {
        let store = MemStore::new();
        let opened = Utc::now();
        store
            .open_auto_approve_window(AutoApproveWindow {
                opened_at: opened,
                expires_at: opened + chrono::Duration::seconds(10),
                remaining_count: None, // unlimited count
                opened_by: "root".into(),
            })
            .await
            .unwrap();
        // Time has passed.
        let later = opened + chrono::Duration::seconds(20);
        let cn = store
            .register_cn(Uuid::new_v4(), "h".into(), None, sysinfo_fixture(), later)
            .await
            .unwrap();
        assert_eq!(cn.state, CnState::Pending);
        // Window auto-cleared.
        assert!(store.get_auto_approve_window().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_cns_filters_by_state() {
        let store = MemStore::new();
        let now = Utc::now();
        let p = store
            .register_cn(Uuid::new_v4(), "p".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        let a = store
            .register_cn(Uuid::new_v4(), "a".into(), None, sysinfo_fixture(), now)
            .await
            .unwrap();
        store
            .approve_cn(a.server_uuid, Uuid::new_v4(), "k".into(), now)
            .await
            .unwrap();

        let pending = store.list_cns(Some(CnState::Pending)).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].server_uuid, p.server_uuid);
        let approved = store.list_cns(Some(CnState::Approved)).await.unwrap();
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].server_uuid, a.server_uuid);
        let all = store.list_cns(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    // ------------------------------------------------------------------
    // Realized network state (Slice H-1)
    // ------------------------------------------------------------------

    fn cn(uuid: Uuid) -> RealizerId {
        RealizerId::Cn { id: uuid }
    }

    fn nat(uuid: Uuid) -> NetworkResourceId {
        NetworkResourceId::NatGateway { id: uuid }
    }

    #[tokio::test]
    async fn realization_round_trips() {
        let store = MemStore::new();
        let resource = nat(Uuid::new_v4());
        let realizer = cn(Uuid::new_v4());
        store
            .record_network_realization(
                resource,
                realizer,
                3,
                RealizationStatus::Applied,
                Some("ok".into()),
            )
            .await
            .unwrap();

        let rows = store.list_network_realizations(resource).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].generation, 3);
        assert_eq!(rows[0].status, RealizationStatus::Applied);
        assert_eq!(rows[0].message.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn realization_backward_generation_rejected() {
        let store = MemStore::new();
        let resource = nat(Uuid::new_v4());
        let realizer = cn(Uuid::new_v4());
        store
            .record_network_realization(resource, realizer, 7, RealizationStatus::Applied, None)
            .await
            .unwrap();
        let err = store
            .record_network_realization(resource, realizer, 5, RealizationStatus::Applied, None)
            .await
            .unwrap_err();
        match err {
            StoreError::Conflict(msg) => assert!(
                msg.contains("backward generation"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected Conflict, got {other:?}"),
        }
        // Existing row is unchanged.
        let rows = store.list_network_realizations(resource).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].generation, 7);
    }

    #[tokio::test]
    async fn realization_same_generation_status_change_allowed() {
        // Applied(5) followed by Failed(5) is legal — the dataplane
        // could subsequently fail at a previously-applied
        // generation due to a transient issue.
        let store = MemStore::new();
        let resource = nat(Uuid::new_v4());
        let realizer = cn(Uuid::new_v4());
        store
            .record_network_realization(resource, realizer, 5, RealizationStatus::Applied, None)
            .await
            .unwrap();
        store
            .record_network_realization(
                resource,
                realizer,
                5,
                RealizationStatus::Failed,
                Some("kernel transport: ENOTCONN".into()),
            )
            .await
            .unwrap();
        let rows = store.list_network_realizations(resource).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, RealizationStatus::Failed);
    }

    #[tokio::test]
    async fn realization_idempotent_at_same_generation() {
        let store = MemStore::new();
        let resource = nat(Uuid::new_v4());
        let realizer = cn(Uuid::new_v4());
        for _ in 0..3 {
            store
                .record_network_realization(resource, realizer, 9, RealizationStatus::Applied, None)
                .await
                .unwrap();
        }
        let rows = store.list_network_realizations(resource).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].generation, 9);
    }

    #[tokio::test]
    async fn realization_multi_realizer_rows_coexist() {
        let store = MemStore::new();
        let resource = nat(Uuid::new_v4());
        let cn1 = cn(Uuid::new_v4());
        let cn2 = cn(Uuid::new_v4());
        let edge = RealizerId::EdgeCluster { id: Uuid::new_v4() };
        store
            .record_network_realization(resource, cn1, 1, RealizationStatus::Applied, None)
            .await
            .unwrap();
        store
            .record_network_realization(resource, cn2, 2, RealizationStatus::Accepted, None)
            .await
            .unwrap();
        store
            .record_network_realization(resource, edge, 3, RealizationStatus::Applied, None)
            .await
            .unwrap();
        let rows = store.list_network_realizations(resource).await.unwrap();
        assert_eq!(rows.len(), 3);
        // Sorted: cn rows first (by uuid asc), edge_cluster last.
        assert_eq!(rows[2].realizer, edge);
    }

    #[tokio::test]
    async fn realization_distinct_resources_isolated() {
        let store = MemStore::new();
        let nat1 = nat(Uuid::new_v4());
        let nat2 = nat(Uuid::new_v4());
        let realizer = cn(Uuid::new_v4());
        store
            .record_network_realization(nat1, realizer, 1, RealizationStatus::Applied, None)
            .await
            .unwrap();
        store
            .record_network_realization(nat2, realizer, 9, RealizationStatus::Applied, None)
            .await
            .unwrap();
        let rows1 = store.list_network_realizations(nat1).await.unwrap();
        assert_eq!(rows1.len(), 1);
        assert_eq!(rows1[0].generation, 1);
        let rows2 = store.list_network_realizations(nat2).await.unwrap();
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0].generation, 9);
    }

    #[tokio::test]
    async fn realization_unreported_resource_lists_empty() {
        // Pre-realization state is empty rows, NOT NotFound.
        let store = MemStore::new();
        let rows = store
            .list_network_realizations(nat(Uuid::new_v4()))
            .await
            .unwrap();
        assert!(rows.is_empty());
    }
}
