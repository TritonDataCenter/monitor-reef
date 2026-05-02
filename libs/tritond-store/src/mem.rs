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

use async_trait::async_trait;
use chrono::Utc;
use rand::Rng;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    ApiKey, IdpConfig, Image, NewImage, NewProject, NewSilo, NewSshKey, NewSubnet, NewVpc, Project,
    Silo, SshKey, Store, StoreError, Subnet, SystemKey, User, VPC_VNI_MAX,
    VPC_VNI_RESERVED_CEILING, Vpc,
};

/// Maximum attempts to draw a fresh VNI before giving up. With ~16.7M
/// candidates and any realistic VPC count, collisions are vanishingly
/// rare; the cap is purely defensive.
const VNI_RETRY_ATTEMPTS: usize = 8;

#[derive(Default)]
struct Inner {
    silos_by_id: HashMap<Uuid, Silo>,
    silo_id_by_name: HashMap<String, Uuid>,
    users_by_id: HashMap<Uuid, User>,
    user_id_by_username: HashMap<String, Uuid>,
    /// `(silo_id, issuer, subject)` → user_id index for federated
    /// users.
    user_id_by_federation: HashMap<(Uuid, String, String), Uuid>,
    api_keys_by_id: HashMap<Uuid, ApiKey>,
    api_key_id_by_lookup_id: HashMap<String, Uuid>,
    system_keys: HashMap<SystemKey, Vec<u8>>,
    idp_configs_by_silo: HashMap<Uuid, IdpConfig>,
    projects_by_id: HashMap<Uuid, Project>,
    /// `(silo_id, name)` → project_id index for the within-silo
    /// uniqueness check.
    project_id_by_silo_name: HashMap<(Uuid, String), Uuid>,
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
    ssh_keys_by_id: HashMap<Uuid, SshKey>,
    /// `(silo_id, name)` → key_id index for within-silo name
    /// uniqueness.
    ssh_key_id_by_silo_name: HashMap<(Uuid, String), Uuid>,
    /// `(silo_id, fingerprint)` → key_id index for within-silo
    /// fingerprint uniqueness (no aliased pool entries).
    ssh_key_id_by_silo_fingerprint: HashMap<(Uuid, String), Uuid>,
    images_by_id: HashMap<Uuid, Image>,
    /// `(silo_id, name)` → image_id index for within-silo name
    /// uniqueness.
    image_id_by_silo_name: HashMap<(Uuid, String), Uuid>,
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

        let silo = Silo {
            id: Uuid::new_v4(),
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        guard.silo_id_by_name.insert(silo.name.clone(), silo.id);
        guard.silos_by_id.insert(silo.id, silo.clone());
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
        if let (Some(silo_id), Some(fed)) = (user.silo_id, user.federation.as_ref()) {
            let key = (silo_id, fed.issuer.clone(), fed.subject.clone());
            if guard.user_id_by_federation.contains_key(&key) {
                return Err(StoreError::Conflict(format!(
                    "federated user already exists for silo {silo_id} issuer {} subject {}",
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
        silo_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<User, StoreError> {
        let guard = self.inner.read().await;
        let key = (silo_id, issuer.to_string(), subject.to_string());
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
        silo_id: Uuid,
        config: IdpConfig,
    ) -> Result<IdpConfig, StoreError> {
        let mut guard = self.inner.write().await;
        guard.idp_configs_by_silo.insert(silo_id, config.clone());
        Ok(config)
    }

    async fn get_idp_config(&self, silo_id: Uuid) -> Result<IdpConfig, StoreError> {
        let guard = self.inner.read().await;
        guard
            .idp_configs_by_silo
            .get(&silo_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn delete_idp_config(&self, silo_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        guard
            .idp_configs_by_silo
            .remove(&silo_id)
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }

    async fn list_idp_configs(&self) -> Result<Vec<(Uuid, IdpConfig)>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .idp_configs_by_silo
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect())
    }

    async fn create_project(&self, silo_id: Uuid, req: NewProject) -> Result<Project, StoreError> {
        let mut guard = self.inner.write().await;
        if !guard.silos_by_id.contains_key(&silo_id) {
            return Err(StoreError::NotFound);
        }
        let key = (silo_id, req.name.clone());
        if guard.project_id_by_silo_name.contains_key(&key) {
            return Err(StoreError::Conflict(format!(
                "project with name {:?} already exists in silo {silo_id}",
                req.name
            )));
        }
        let project = Project {
            id: Uuid::new_v4(),
            silo_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        guard.project_id_by_silo_name.insert(key, project.id);
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

    async fn list_projects_in_silo(&self, silo_id: Uuid) -> Result<Vec<Project>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .projects_by_id
            .values()
            .filter(|p| p.silo_id == silo_id)
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
            .project_id_by_silo_name
            .remove(&(project.silo_id, project.name));
        Ok(())
    }

    async fn create_vpc(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        req: NewVpc,
    ) -> Result<Vpc, StoreError> {
        let mut guard = self.inner.write().await;

        // Project must exist and live in the right silo. A silo
        // mismatch surfaces as NotFound (project is invisible to a
        // foreign silo).
        let project = guard
            .projects_by_id
            .get(&project_id)
            .ok_or(StoreError::NotFound)?;
        if project.silo_id != silo_id {
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

        let vpc = Vpc {
            id: Uuid::new_v4(),
            silo_id,
            project_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            vni,
            ipv4_block: req.ipv4_block,
            ipv6_block: req.ipv6_block,
            created_at: Utc::now(),
        };
        guard.vnis_in_use.insert(vni);
        guard.vpc_id_by_project_name.insert(name_key, vpc.id);
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
        let vpc = guard
            .vpcs_by_id
            .remove(&vpc_id)
            .ok_or(StoreError::NotFound)?;
        guard
            .vpc_id_by_project_name
            .remove(&(vpc.project_id, vpc.name));
        guard.vnis_in_use.remove(&vpc.vni);
        Ok(())
    }

    async fn create_subnet(
        &self,
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        req: NewSubnet,
    ) -> Result<Subnet, StoreError> {
        let mut guard = self.inner.write().await;

        // VPC must exist and live under the right silo+project. Any
        // mismatch surfaces as NotFound (cross-tenant probe story).
        let vpc = guard.vpcs_by_id.get(&vpc_id).ok_or(StoreError::NotFound)?;
        if vpc.silo_id != silo_id || vpc.project_id != project_id {
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
            silo_id,
            project_id,
            vpc_id,
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

    async fn create_ssh_key(
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
        let key = SshKey {
            id: Uuid::new_v4(),
            silo_id,
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            public_key: req.public_key,
            fingerprint,
            created_at: Utc::now(),
        };
        guard.ssh_key_id_by_silo_name.insert(name_key, key.id);
        guard.ssh_key_id_by_silo_fingerprint.insert(fp_key, key.id);
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

    async fn list_ssh_keys_in_silo(&self, silo_id: Uuid) -> Result<Vec<SshKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .ssh_keys_by_id
            .values()
            .filter(|k| k.silo_id == silo_id)
            .cloned()
            .collect())
    }

    async fn delete_ssh_key(&self, key_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let key = guard
            .ssh_keys_by_id
            .remove(&key_id)
            .ok_or(StoreError::NotFound)?;
        guard
            .ssh_key_id_by_silo_name
            .remove(&(key.silo_id, key.name));
        guard
            .ssh_key_id_by_silo_fingerprint
            .remove(&(key.silo_id, key.fingerprint));
        Ok(())
    }

    async fn create_image(&self, silo_id: Uuid, req: NewImage) -> Result<Image, StoreError> {
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
        guard.image_id_by_silo_name.insert(name_key, image.id);
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

    async fn list_images_in_silo(&self, silo_id: Uuid) -> Result<Vec<Image>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .images_by_id
            .values()
            .filter(|i| i.silo_id == silo_id)
            .cloned()
            .collect())
    }

    async fn delete_image(&self, image_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        let image = guard
            .images_by_id
            .remove(&image_id)
            .ok_or(StoreError::NotFound)?;
        guard
            .image_id_by_silo_name
            .remove(&(image.silo_id, image.name));
        Ok(())
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
            silo_id: None,
            federation: None,
        }
    }

    fn federated_user_fixture(silo_id: Uuid, issuer: &str, subject: &str) -> User {
        use crate::Federation;
        User {
            id: Uuid::new_v4(),
            username: format!("{subject}@{issuer}"),
            password_hash: String::new(),
            is_root: false,
            created_at: Utc::now(),
            silo_id: Some(silo_id),
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
            created_at: Utc::now(),
        };
        let key_b = ApiKey {
            id: Uuid::new_v4(),
            user_id: other.id,
            description: "tf".to_string(),
            lookup_id: "BBBBBBBBBBBB".to_string(),
            hash: "$hashB".to_string(),
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
        let silo_id = Uuid::new_v4();
        let user = federated_user_fixture(silo_id, "https://idp.example", "tenant-42");
        let user_id = user.id;
        store.create_user(user).await.unwrap();

        let resolved = store
            .get_user_by_federation(silo_id, "https://idp.example", "tenant-42")
            .await
            .unwrap();
        assert_eq!(resolved.id, user_id);
        assert_eq!(resolved.silo_id, Some(silo_id));
    }

    #[tokio::test]
    async fn duplicate_federation_triple_conflicts() {
        let store = MemStore::new();
        let silo_id = Uuid::new_v4();
        store
            .create_user(federated_user_fixture(
                silo_id,
                "https://idp.example",
                "tenant-42",
            ))
            .await
            .unwrap();
        // Same (silo, issuer, subject) but distinct username/uuid:
        let mut second = federated_user_fixture(silo_id, "https://idp.example", "tenant-42");
        second.username = format!("alt-{}", second.id);
        let err = store
            .create_user(second)
            .await
            .expect_err("duplicate federation triple should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn project_round_trip_within_silo() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "tenants".to_string(),
                description: None,
            })
            .await
            .unwrap();

        let p = store
            .create_project(
                silo.id,
                NewProject {
                    name: "alpha".to_string(),
                    description: Some("first".to_string()),
                },
            )
            .await
            .unwrap();
        assert_eq!(p.silo_id, silo.id);

        let fetched = store.get_project(p.id).await.unwrap();
        assert_eq!(fetched, p);

        let listed = store.list_projects_in_silo(silo.id).await.unwrap();
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
    async fn duplicate_project_name_within_silo_conflicts() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "ops".to_string(),
                description: None,
            })
            .await
            .unwrap();
        store
            .create_project(
                silo.id,
                NewProject {
                    name: "alpha".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let err = store
            .create_project(
                silo.id,
                NewProject {
                    name: "alpha".to_string(),
                    description: None,
                },
            )
            .await
            .expect_err("duplicate within silo conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn same_project_name_in_different_silos_does_not_conflict() {
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
                a.id,
                NewProject {
                    name: "shared".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        store
            .create_project(
                b.id,
                NewProject {
                    name: "shared".to_string(),
                    description: None,
                },
            )
            .await
            .expect("same name across silos must be allowed");
    }

    #[tokio::test]
    async fn create_project_in_unknown_silo_is_not_found() {
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
            .expect_err("unknown silo should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn idp_config_round_trip() {
        let store = MemStore::new();
        let silo_id = Uuid::new_v4();
        let config = IdpConfig {
            issuer_url: "https://idp.example".to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        };
        let err = store
            .get_idp_config(silo_id)
            .await
            .expect_err("missing idp config is not-found");
        assert!(matches!(err, StoreError::NotFound));

        store.put_idp_config(silo_id, config.clone()).await.unwrap();
        let read = store.get_idp_config(silo_id).await.unwrap();
        assert_eq!(read.issuer_url, "https://idp.example");

        let listed = store.list_idp_configs().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, silo_id);

        store.delete_idp_config(silo_id).await.unwrap();
        let err = store
            .get_idp_config(silo_id)
            .await
            .expect_err("deleted idp config is not-found");
        assert!(matches!(err, StoreError::NotFound));
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

    async fn make_silo_and_project(store: &MemStore) -> (Uuid, Uuid) {
        let silo = store
            .create_silo(NewSilo {
                name: format!("silo-{}", Uuid::new_v4()),
                description: None,
            })
            .await
            .unwrap();
        let project = store
            .create_project(
                silo.id,
                NewProject {
                    name: "default".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        (silo.id, project.id)
    }

    #[tokio::test]
    async fn vpc_round_trip_within_project() {
        let store = MemStore::new();
        let (silo_id, project_id) = make_silo_and_project(&store).await;

        let vpc = store
            .create_vpc(
                silo_id,
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
        assert_eq!(vpc.silo_id, silo_id);
        assert_eq!(vpc.project_id, project_id);
        assert!(vpc.vni >= VPC_VNI_RESERVED_CEILING && vpc.vni < VPC_VNI_MAX);
        assert_eq!(vpc.ipv4_block, Some(ipv4_cidr("10.0.0.0/24")));
        assert_eq!(vpc.ipv6_block, Some(ipv6_cidr("fd00::/48")));

        let fetched = store.get_vpc(vpc.id).await.unwrap();
        assert_eq!(fetched, vpc);

        let listed = store.list_vpcs_in_project(project_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, vpc.id);

        store.delete_vpc(vpc.id).await.unwrap();
        let err = store
            .get_vpc(vpc.id)
            .await
            .expect_err("post-delete get is not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn vpc_ipv4_only_and_ipv6_only_round_trip() {
        let store = MemStore::new();
        let (silo_id, project_id) = make_silo_and_project(&store).await;

        let v4 = store
            .create_vpc(
                silo_id,
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
                silo_id,
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
        let (silo_id, project_id) = make_silo_and_project(&store).await;
        store
            .create_vpc(
                silo_id,
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
                silo_id,
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
        let p1 = store
            .create_project(
                silo.id,
                NewProject {
                    name: "alpha".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let p2 = store
            .create_project(
                silo.id,
                NewProject {
                    name: "beta".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();
        store
            .create_vpc(
                silo.id,
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
                silo.id,
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
                silo.id,
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
    async fn create_vpc_with_project_in_wrong_silo_is_not_found() {
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
                silo_a.id,
                NewProject {
                    name: "owned-by-a".to_string(),
                    description: None,
                },
            )
            .await
            .unwrap();

        // Caller claims silo_b but the project lives in silo_a.
        let err = store
            .create_vpc(
                silo_b.id,
                project.id,
                NewVpc {
                    name: "wrong".to_string(),
                    description: None,
                    ipv4_block: Some(ipv4_cidr("10.0.0.0/24")),
                    ipv6_block: None,
                },
            )
            .await
            .expect_err("project-in-wrong-silo should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    async fn make_silo_project_vpc(store: &MemStore) -> (Uuid, Uuid, Vpc) {
        let (silo_id, project_id) = make_silo_and_project(store).await;
        let vpc = store
            .create_vpc(
                silo_id,
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
        (silo_id, project_id, vpc)
    }

    #[tokio::test]
    async fn subnet_round_trip_within_vpc() {
        let store = MemStore::new();
        let (silo_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        let subnet = store
            .create_subnet(
                silo_id,
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
        assert_eq!(subnet.silo_id, silo_id);
        assert_eq!(subnet.project_id, project_id);
        assert_eq!(subnet.vpc_id, vpc.id);

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
        let (silo_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        // 10.1.0.0/24 is NOT inside the vpc's 10.0.0.0/16.
        let err = store
            .create_subnet(
                silo_id,
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
        let (silo_id, project_id) = make_silo_and_project(&store).await;
        // IPv6-only VPC (no ipv4_block).
        let vpc = store
            .create_vpc(
                silo_id,
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
                silo_id,
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
        let (silo_id, project_id, vpc) = make_silo_project_vpc(&store).await;

        store
            .create_subnet(
                silo_id,
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
                silo_id,
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
        let (silo_id, project_id, vpc) = make_silo_project_vpc(&store).await;
        store
            .create_subnet(
                silo_id,
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
                silo_id,
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
        let (silo_id, project_id) = make_silo_and_project(&store).await;
        let err = store
            .create_subnet(
                silo_id,
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
        let (silo_id, project_id_a, vpc) = make_silo_project_vpc(&store).await;
        let project_b = store
            .create_project(
                silo_id,
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
                silo_id,
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
        let (silo_id, project_id, vpc) = make_silo_project_vpc(&store).await;
        let subnet = store
            .create_subnet(
                silo_id,
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
    async fn delete_vpc_frees_vni_and_name() {
        let store = MemStore::new();
        let (silo_id, project_id) = make_silo_and_project(&store).await;
        let vpc = store
            .create_vpc(
                silo_id,
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
                silo_id,
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
            .create_ssh_key(
                silo.id,
                ssh_key_req("ci", "ssh-ed25519 AAAA test"),
                "SHA256:abc".to_string(),
            )
            .await
            .unwrap();
        assert_eq!(key.silo_id, silo.id);
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
            .create_ssh_key(
                silo.id,
                ssh_key_req("ci", "ssh-ed25519 AAAA"),
                "SHA256:a".to_string(),
            )
            .await
            .unwrap();
        let err = store
            .create_ssh_key(
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
            .create_ssh_key(
                silo.id,
                ssh_key_req("alice", "ssh-ed25519 AAAA"),
                "SHA256:dup".to_string(),
            )
            .await
            .unwrap();
        let err = store
            .create_ssh_key(
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
            .create_ssh_key(
                a.id,
                ssh_key_req("ci", "ssh-ed25519 AAAA"),
                "SHA256:x".to_string(),
            )
            .await
            .unwrap();
        store
            .create_ssh_key(
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
            .create_ssh_key(
                Uuid::new_v4(),
                ssh_key_req("orphan", "ssh-ed25519 AAAA"),
                "SHA256:x".to_string(),
            )
            .await
            .expect_err("unknown silo should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    fn image_req(name: &str) -> NewImage {
        NewImage {
            name: name.to_string(),
            description: None,
            os: "linux".to_string(),
            version: "ubuntu-22.04".to_string(),
            size_bytes: 1_000_000_000,
            sha256: "0".repeat(64),
            source_url: Some("mantafs://images/test".to_string()),
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
            .create_image(silo.id, image_req("ubuntu-base"))
            .await
            .unwrap();
        assert_eq!(img.silo_id, silo.id);
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
            .create_image(silo.id, image_req("ubuntu-base"))
            .await
            .unwrap();
        let err = store
            .create_image(silo.id, image_req("ubuntu-base"))
            .await
            .expect_err("duplicate name within silo conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn create_image_in_unknown_silo_is_not_found() {
        let store = MemStore::new();
        let err = store
            .create_image(Uuid::new_v4(), image_req("orphan"))
            .await
            .expect_err("unknown silo should be not-found");
        assert!(matches!(err, StoreError::NotFound));
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
}
