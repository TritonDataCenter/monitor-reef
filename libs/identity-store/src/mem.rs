// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-process [`IdentityStore`] backend.
//!
//! A single `Mutex<Inner>` over plain `HashMap`s. Used for tests and for
//! `identityd` runs that don't need durable state. Lookups that the FDB
//! backend serves with reverse indexes are linear scans here — `MemStore`
//! is for correctness and tests, not throughput.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::*;
use crate::{IdentityStore, StoreError};

/// In-memory identity store.
#[derive(Default)]
pub struct MemStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    realms: HashMap<Uuid, Realm>,
    users: HashMap<Uuid, User>,
    groups: HashMap<Uuid, Group>,
    group_members: HashMap<Uuid, HashSet<Uuid>>, // group_id -> {user_id}
    role_assignments: HashMap<Uuid, RoleAssignment>,
    oauth_clients: HashMap<Uuid, OAuthClient>,
    connections: HashMap<Uuid, UpstreamConnection>,
    claim_mappings: HashMap<Uuid, Vec<ClaimMapping>>, // connection_id -> mappings
    signing_keys: HashMap<(Uuid, String), SigningKey>,
    rotation_lock: Option<RotationLock>,
    auth_codes: HashMap<String, AuthCode>,
    refresh_tokens: HashMap<Uuid, RefreshToken>,
    device_codes: HashMap<String, DeviceCode>, // keyed by device_code
    sessions: HashMap<Uuid, Session>,
    broker_states: HashMap<String, BrokerState>,
}

impl MemStore {
    /// Fresh, empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a prior panic inside a critical section.
        // Recover the guard rather than cascading the panic — the data is
        // still structurally fine (every write here is a single map op).
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Structural-scope rule for role assignments (see [`IdentityStore::create_role_assignment`]).
fn check_grant_scope(realm_scope: &RealmScope, target: &AssignmentTarget) -> Result<(), String> {
    use AssignmentTarget as T;
    use RealmScope as S;
    match (realm_scope, target) {
        (S::System, T::Fleet) => Ok(()),
        (S::System, _) => Err("the System realm may only grant Fleet-scoped roles".into()),
        (_, T::Fleet) => Err("only the System realm may grant Fleet-scoped roles".into()),
        (S::Silo { silo_id: rs }, T::Silo { silo_id: ts }) if rs == ts => Ok(()),
        (S::Silo { .. }, T::Silo { .. }) => {
            Err("a silo realm may only grant roles over its own silo".into())
        }
        // The cross-silo check (tenant must belong to this silo) needs the
        // tenant→silo mapping, which lives in tritond-store, not here — it
        // is identityd's responsibility, not the store's.
        (S::Silo { .. }, T::Tenant { .. }) => Ok(()),
        (S::Tenant { tenant_id: rt }, T::Tenant { tenant_id: tt }) if rt == tt => Ok(()),
        (S::Tenant { .. }, _) => {
            Err("a tenant realm may only grant roles over its own tenant".into())
        }
    }
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

#[async_trait]
impl IdentityStore for MemStore {
    // ---------------- Realms ----------------

    async fn create_realm(
        &self,
        req: NewRealm,
        initial_keys: Vec<NewSigningKey>,
    ) -> Result<Realm, StoreError> {
        if initial_keys.is_empty() {
            return Err(StoreError::Conflict(
                "a realm must be created with at least one signing key".into(),
            ));
        }
        let mut inner = self.lock();
        if inner
            .realms
            .values()
            .any(|r| r.issuer_url == req.issuer_url)
        {
            return Err(StoreError::Conflict(format!(
                "issuer {:?} is already claimed by another realm",
                req.issuer_url
            )));
        }
        if inner.realms.values().any(|r| r.scope == req.scope) {
            return Err(StoreError::Conflict(format!(
                "a realm with scope {} already exists",
                req.scope.tag()
            )));
        }
        // kid uniqueness within the seed ring.
        let mut seen_kids = HashSet::new();
        for k in &initial_keys {
            if !seen_kids.insert(k.kid.clone()) {
                return Err(StoreError::Conflict(format!(
                    "duplicate kid {:?} in initial key ring",
                    k.kid
                )));
            }
        }
        let id = Uuid::new_v4();
        let created_at = now();
        let realm = Realm {
            id,
            scope: req.scope,
            name: req.name,
            description: req.description.unwrap_or_default(),
            issuer_url: req.issuer_url,
            signing_alg: req.signing_alg.unwrap_or_default(),
            access_token_ttl_secs: req
                .access_token_ttl_secs
                .unwrap_or(DEFAULT_ACCESS_TOKEN_TTL_SECS),
            id_token_ttl_secs: req.id_token_ttl_secs.unwrap_or(DEFAULT_ID_TOKEN_TTL_SECS),
            refresh_token_ttl_secs: req
                .refresh_token_ttl_secs
                .unwrap_or(DEFAULT_REFRESH_TOKEN_TTL_SECS),
            auth_code_ttl_secs: req.auth_code_ttl_secs.unwrap_or(DEFAULT_AUTH_CODE_TTL_SECS),
            device_code_ttl_secs: req
                .device_code_ttl_secs
                .unwrap_or(DEFAULT_DEVICE_CODE_TTL_SECS),
            login_policy: req.login_policy.unwrap_or_default(),
            created_at,
        };
        for k in initial_keys {
            let key = SigningKey {
                kid: k.kid,
                realm_id: id,
                alg: k.alg,
                private_pem: k.private_pem,
                public_jwk: k.public_jwk,
                status: k.status,
                not_before: k.not_before,
                not_after: k.not_after,
                created_at,
            };
            inner.signing_keys.insert((id, key.kid.clone()), key);
        }
        inner.realms.insert(id, realm.clone());
        Ok(realm)
    }

    async fn get_realm(&self, id: Uuid) -> Result<Realm, StoreError> {
        self.lock()
            .realms
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_realm_by_issuer(&self, issuer: &str) -> Result<Realm, StoreError> {
        self.lock()
            .realms
            .values()
            .find(|r| r.issuer_url == issuer)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_realm_by_scope(&self, scope: &RealmScope) -> Result<Realm, StoreError> {
        self.lock()
            .realms
            .values()
            .find(|r| &r.scope == scope)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_realms(&self) -> Result<Vec<Realm>, StoreError> {
        Ok(self.lock().realms.values().cloned().collect())
    }

    async fn update_realm_settings(
        &self,
        id: Uuid,
        settings: RealmSettings,
    ) -> Result<Realm, StoreError> {
        let mut inner = self.lock();
        let r = inner.realms.get_mut(&id).ok_or(StoreError::NotFound)?;
        r.access_token_ttl_secs = settings.access_token_ttl_secs;
        r.id_token_ttl_secs = settings.id_token_ttl_secs;
        r.refresh_token_ttl_secs = settings.refresh_token_ttl_secs;
        r.auth_code_ttl_secs = settings.auth_code_ttl_secs;
        r.device_code_ttl_secs = settings.device_code_ttl_secs;
        r.login_policy = settings.login_policy;
        Ok(r.clone())
    }

    async fn delete_realm(&self, id: Uuid) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if !inner.realms.contains_key(&id) {
            return Err(StoreError::NotFound);
        }
        if inner.users.values().any(|u| u.realm_id == id)
            || inner.oauth_clients.values().any(|c| c.realm_id == id)
            || inner.connections.values().any(|c| c.realm_id == id)
        {
            return Err(StoreError::Conflict(
                "realm still has users, clients, or connections".into(),
            ));
        }
        inner.realms.remove(&id);
        inner.signing_keys.retain(|(rid, _), _| *rid != id);
        Ok(())
    }

    // ---------------- Users ----------------

    async fn create_user(&self, req: NewUser) -> Result<User, StoreError> {
        let mut inner = self.lock();
        if !inner.realms.contains_key(&req.realm_id) {
            return Err(StoreError::NotFound);
        }
        if inner
            .users
            .values()
            .any(|u| u.realm_id == req.realm_id && u.username == req.username)
        {
            return Err(StoreError::Conflict(format!(
                "username {:?} already exists in this realm",
                req.username
            )));
        }
        if let Some(email) = &req.email
            && inner
                .users
                .values()
                .any(|u| u.realm_id == req.realm_id && u.email.as_deref() == Some(email.as_str()))
        {
            return Err(StoreError::Conflict(format!(
                "email {email:?} already exists in this realm"
            )));
        }
        let user = User {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            display_name: req.display_name.unwrap_or_else(|| req.username.clone()),
            username: req.username,
            email: req.email,
            password_hash: req.password_hash,
            is_root: req.is_root,
            fleet_admin: req.fleet_admin,
            status: UserStatus::Active,
            mfa: None,
            brokered: req.brokered,
            created_at: now(),
        };
        inner.users.insert(user.id, user.clone());
        Ok(user)
    }

    async fn get_user(&self, id: Uuid) -> Result<User, StoreError> {
        self.lock()
            .users
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_user_by_username(
        &self,
        realm_id: Uuid,
        username: &str,
    ) -> Result<User, StoreError> {
        self.lock()
            .users
            .values()
            .find(|u| u.realm_id == realm_id && u.username == username)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_user_by_brokered(
        &self,
        realm_id: Uuid,
        connection_id: Uuid,
        upstream_subject: &str,
    ) -> Result<User, StoreError> {
        self.lock()
            .users
            .values()
            .find(|u| {
                u.realm_id == realm_id
                    && u.brokered.as_ref().is_some_and(|b| {
                        b.connection_id == connection_id && b.upstream_subject == upstream_subject
                    })
            })
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_users_in_realm(&self, realm_id: Uuid) -> Result<Vec<User>, StoreError> {
        Ok(self
            .lock()
            .users
            .values()
            .filter(|u| u.realm_id == realm_id)
            .cloned()
            .collect())
    }

    async fn update_user_password_hash(
        &self,
        id: Uuid,
        password_hash: String,
    ) -> Result<User, StoreError> {
        let mut inner = self.lock();
        let u = inner.users.get_mut(&id).ok_or(StoreError::NotFound)?;
        u.password_hash = password_hash;
        Ok(u.clone())
    }

    async fn set_user_status(&self, id: Uuid, status: UserStatus) -> Result<User, StoreError> {
        let mut inner = self.lock();
        let u = inner.users.get_mut(&id).ok_or(StoreError::NotFound)?;
        u.status = status;
        let updated = u.clone();
        if status == UserStatus::Disabled {
            for t in inner.refresh_tokens.values_mut() {
                if t.user_id == id {
                    t.revoked = true;
                }
            }
        }
        Ok(updated)
    }

    async fn delete_user(&self, id: Uuid) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.users.remove(&id).is_none() {
            return Err(StoreError::NotFound);
        }
        for members in inner.group_members.values_mut() {
            members.remove(&id);
        }
        inner.refresh_tokens.retain(|_, t| t.user_id != id);
        inner.sessions.retain(|_, s| s.user_id != id);
        inner
            .role_assignments
            .retain(|_, a| a.subject != AssignmentSubject::User { user_id: id });
        Ok(())
    }

    async fn has_any_user_in_realm(&self, realm_id: Uuid) -> Result<bool, StoreError> {
        Ok(self.lock().users.values().any(|u| u.realm_id == realm_id))
    }

    // ---------------- Groups ----------------

    async fn create_group(&self, req: NewGroup) -> Result<Group, StoreError> {
        let mut inner = self.lock();
        if !inner.realms.contains_key(&req.realm_id) {
            return Err(StoreError::NotFound);
        }
        if inner
            .groups
            .values()
            .any(|g| g.realm_id == req.realm_id && g.name == req.name)
        {
            return Err(StoreError::Conflict(format!(
                "group {:?} already exists in this realm",
                req.name
            )));
        }
        let g = Group {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: now(),
        };
        inner.groups.insert(g.id, g.clone());
        Ok(g)
    }

    async fn get_group(&self, id: Uuid) -> Result<Group, StoreError> {
        self.lock()
            .groups
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_groups_in_realm(&self, realm_id: Uuid) -> Result<Vec<Group>, StoreError> {
        Ok(self
            .lock()
            .groups
            .values()
            .filter(|g| g.realm_id == realm_id)
            .cloned()
            .collect())
    }

    async fn delete_group(&self, id: Uuid) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.groups.remove(&id).is_none() {
            return Err(StoreError::NotFound);
        }
        inner.group_members.remove(&id);
        inner
            .role_assignments
            .retain(|_, a| a.subject != AssignmentSubject::Group { group_id: id });
        Ok(())
    }

    async fn add_group_member(&self, group_id: Uuid, user_id: Uuid) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if !inner.groups.contains_key(&group_id) || !inner.users.contains_key(&user_id) {
            return Err(StoreError::NotFound);
        }
        inner
            .group_members
            .entry(group_id)
            .or_default()
            .insert(user_id);
        Ok(())
    }

    async fn remove_group_member(&self, group_id: Uuid, user_id: Uuid) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if !inner.groups.contains_key(&group_id) || !inner.users.contains_key(&user_id) {
            return Err(StoreError::NotFound);
        }
        if let Some(m) = inner.group_members.get_mut(&group_id) {
            m.remove(&user_id);
        }
        Ok(())
    }

    async fn list_group_members(&self, group_id: Uuid) -> Result<Vec<Uuid>, StoreError> {
        let inner = self.lock();
        if !inner.groups.contains_key(&group_id) {
            return Err(StoreError::NotFound);
        }
        Ok(inner
            .group_members
            .get(&group_id)
            .map(|m| m.iter().copied().collect())
            .unwrap_or_default())
    }

    async fn list_groups_of_user(&self, user_id: Uuid) -> Result<Vec<Uuid>, StoreError> {
        let inner = self.lock();
        if !inner.users.contains_key(&user_id) {
            return Err(StoreError::NotFound);
        }
        Ok(inner
            .group_members
            .iter()
            .filter(|(_, m)| m.contains(&user_id))
            .map(|(gid, _)| *gid)
            .collect())
    }

    // ---------------- Role assignments ----------------

    async fn create_role_assignment(
        &self,
        req: NewRoleAssignment,
    ) -> Result<RoleAssignment, StoreError> {
        let mut inner = self.lock();
        let realm = inner
            .realms
            .get(&req.realm_id)
            .ok_or(StoreError::NotFound)?;
        check_grant_scope(&realm.scope, &req.target).map_err(StoreError::Conflict)?;
        // subject sanity: a user/group subject should exist in this realm.
        match &req.subject {
            AssignmentSubject::User { user_id } => {
                let ok = inner
                    .users
                    .get(user_id)
                    .is_some_and(|u| u.realm_id == req.realm_id);
                if !ok {
                    return Err(StoreError::NotFound);
                }
            }
            AssignmentSubject::Group { group_id } => {
                let ok = inner
                    .groups
                    .get(group_id)
                    .is_some_and(|g| g.realm_id == req.realm_id);
                if !ok {
                    return Err(StoreError::NotFound);
                }
            }
        }
        if inner.role_assignments.values().any(|a| {
            a.realm_id == req.realm_id
                && a.subject == req.subject
                && a.target == req.target
                && a.role == req.role
        }) {
            return Err(StoreError::Conflict(
                "an identical role assignment already exists".into(),
            ));
        }
        let a = RoleAssignment {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            subject: req.subject,
            target: req.target,
            role: req.role,
            created_at: now(),
            created_by: req.created_by,
        };
        inner.role_assignments.insert(a.id, a.clone());
        Ok(a)
    }

    async fn get_role_assignment(&self, id: Uuid) -> Result<RoleAssignment, StoreError> {
        self.lock()
            .role_assignments
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_assignments_of_subject(
        &self,
        subject: &AssignmentSubject,
    ) -> Result<Vec<RoleAssignment>, StoreError> {
        Ok(self
            .lock()
            .role_assignments
            .values()
            .filter(|a| &a.subject == subject)
            .cloned()
            .collect())
    }

    async fn list_assignments_for_target(
        &self,
        target: &AssignmentTarget,
    ) -> Result<Vec<RoleAssignment>, StoreError> {
        Ok(self
            .lock()
            .role_assignments
            .values()
            .filter(|a| &a.target == target)
            .cloned()
            .collect())
    }

    async fn delete_role_assignment(&self, id: Uuid) -> Result<(), StoreError> {
        self.lock()
            .role_assignments
            .remove(&id)
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }

    // ---------------- OAuth clients ----------------

    async fn create_oauth_client(&self, req: NewOAuthClient) -> Result<OAuthClient, StoreError> {
        let mut inner = self.lock();
        if !inner.realms.contains_key(&req.realm_id) {
            return Err(StoreError::NotFound);
        }
        if let Some(cn) = req.bound_to_cn
            && inner
                .oauth_clients
                .values()
                .any(|c| c.bound_to_cn == Some(cn))
        {
            return Err(StoreError::Conflict(format!(
                "compute node {cn} already has a bound client"
            )));
        }
        let c = OAuthClient {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            name: req.name,
            client_secret_hash: req.client_secret_hash,
            redirect_uris: req.redirect_uris,
            grant_types: req.grant_types,
            pkce_required: req.pkce_required,
            scopes_allowed: req.scopes_allowed,
            is_workload: req.is_workload,
            bound_to_cn: req.bound_to_cn,
            created_at: now(),
        };
        inner.oauth_clients.insert(c.id, c.clone());
        Ok(c)
    }

    async fn get_oauth_client(&self, id: Uuid) -> Result<OAuthClient, StoreError> {
        self.lock()
            .oauth_clients
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_oauth_client_by_cn(&self, server_uuid: Uuid) -> Result<OAuthClient, StoreError> {
        self.lock()
            .oauth_clients
            .values()
            .find(|c| c.bound_to_cn == Some(server_uuid))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_oauth_clients_in_realm(
        &self,
        realm_id: Uuid,
    ) -> Result<Vec<OAuthClient>, StoreError> {
        Ok(self
            .lock()
            .oauth_clients
            .values()
            .filter(|c| c.realm_id == realm_id)
            .cloned()
            .collect())
    }

    async fn update_oauth_client(
        &self,
        id: Uuid,
        update: OAuthClientUpdate,
    ) -> Result<OAuthClient, StoreError> {
        let mut inner = self.lock();
        let c = inner
            .oauth_clients
            .get_mut(&id)
            .ok_or(StoreError::NotFound)?;
        c.name = update.name;
        c.client_secret_hash = update.client_secret_hash;
        c.redirect_uris = update.redirect_uris;
        c.grant_types = update.grant_types;
        c.pkce_required = update.pkce_required;
        c.scopes_allowed = update.scopes_allowed;
        Ok(c.clone())
    }

    async fn delete_oauth_client(&self, id: Uuid) -> Result<(), StoreError> {
        self.lock()
            .oauth_clients
            .remove(&id)
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }

    // ---------------- Upstream connections ----------------

    async fn create_upstream_connection(
        &self,
        req: NewUpstreamConnection,
    ) -> Result<UpstreamConnection, StoreError> {
        let mut inner = self.lock();
        if !inner.realms.contains_key(&req.realm_id) {
            return Err(StoreError::NotFound);
        }
        if inner
            .connections
            .values()
            .any(|c| c.realm_id == req.realm_id && c.name == req.name)
        {
            return Err(StoreError::Conflict(format!(
                "connection {:?} already exists in this realm",
                req.name
            )));
        }
        let c = UpstreamConnection {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            name: req.name,
            kind: req.kind,
            enabled: req.enabled,
            created_at: now(),
        };
        inner.connections.insert(c.id, c.clone());
        Ok(c)
    }

    async fn get_upstream_connection(&self, id: Uuid) -> Result<UpstreamConnection, StoreError> {
        self.lock()
            .connections
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_connections_in_realm(
        &self,
        realm_id: Uuid,
    ) -> Result<Vec<UpstreamConnection>, StoreError> {
        Ok(self
            .lock()
            .connections
            .values()
            .filter(|c| c.realm_id == realm_id)
            .cloned()
            .collect())
    }

    async fn set_connection_enabled(
        &self,
        id: Uuid,
        enabled: bool,
    ) -> Result<UpstreamConnection, StoreError> {
        let mut inner = self.lock();
        // A realm has at most one enabled upstream connection. Enabling one
        // disables every other in the same realm so the identity-source
        // selection is deterministic (no order-dependent "first enabled").
        let realm_id = inner
            .connections
            .get(&id)
            .ok_or(StoreError::NotFound)?
            .realm_id;
        if enabled {
            for other in inner.connections.values_mut() {
                if other.id != id && other.realm_id == realm_id {
                    other.enabled = false;
                }
            }
        }
        let c = inner
            .connections
            .get_mut(&id)
            .ok_or(StoreError::NotFound)?;
        c.enabled = enabled;
        Ok(c.clone())
    }

    async fn delete_upstream_connection(&self, id: Uuid) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.connections.remove(&id).is_none() {
            return Err(StoreError::NotFound);
        }
        inner.claim_mappings.remove(&id);
        Ok(())
    }

    async fn put_claim_mappings(
        &self,
        connection_id: Uuid,
        mappings: Vec<ClaimMapping>,
    ) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if !inner.connections.contains_key(&connection_id) {
            return Err(StoreError::NotFound);
        }
        inner.claim_mappings.insert(connection_id, mappings);
        Ok(())
    }

    async fn list_claim_mappings(
        &self,
        connection_id: Uuid,
    ) -> Result<Vec<ClaimMapping>, StoreError> {
        let inner = self.lock();
        if !inner.connections.contains_key(&connection_id) {
            return Err(StoreError::NotFound);
        }
        Ok(inner
            .claim_mappings
            .get(&connection_id)
            .cloned()
            .unwrap_or_default())
    }

    // ---------------- Signing keys ----------------

    async fn add_signing_key(
        &self,
        realm_id: Uuid,
        key: NewSigningKey,
    ) -> Result<SigningKey, StoreError> {
        let mut inner = self.lock();
        if !inner.realms.contains_key(&realm_id) {
            return Err(StoreError::NotFound);
        }
        if inner
            .signing_keys
            .contains_key(&(realm_id, key.kid.clone()))
        {
            return Err(StoreError::Conflict(format!(
                "kid {:?} already exists in this realm",
                key.kid
            )));
        }
        let sk = SigningKey {
            kid: key.kid,
            realm_id,
            alg: key.alg,
            private_pem: key.private_pem,
            public_jwk: key.public_jwk,
            status: key.status,
            not_before: key.not_before,
            not_after: key.not_after,
            created_at: now(),
        };
        inner
            .signing_keys
            .insert((realm_id, sk.kid.clone()), sk.clone());
        Ok(sk)
    }

    async fn get_signing_key(&self, realm_id: Uuid, kid: &str) -> Result<SigningKey, StoreError> {
        self.lock()
            .signing_keys
            .get(&(realm_id, kid.to_string()))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_signing_keys(&self, realm_id: Uuid) -> Result<Vec<SigningKey>, StoreError> {
        let inner = self.lock();
        if !inner.realms.contains_key(&realm_id) {
            return Err(StoreError::NotFound);
        }
        let mut keys: Vec<SigningKey> = inner
            .signing_keys
            .iter()
            .filter(|((rid, _), _)| *rid == realm_id)
            .map(|(_, k)| k.clone())
            .collect();
        // Lexicographic kid order — matches the FDB `key/in_realm/<realm>/<kid>`
        // index's natural iteration order, and is deterministic regardless of
        // sub-millisecond timestamp ties on `created_at`.
        keys.sort_by(|a, b| a.kid.cmp(&b.kid));
        Ok(keys)
    }

    async fn set_signing_key_status(
        &self,
        realm_id: Uuid,
        kid: &str,
        status: KeyStatus,
    ) -> Result<SigningKey, StoreError> {
        let mut inner = self.lock();
        let k = inner
            .signing_keys
            .get_mut(&(realm_id, kid.to_string()))
            .ok_or(StoreError::NotFound)?;
        k.status = status;
        Ok(k.clone())
    }

    async fn delete_signing_key(&self, realm_id: Uuid, kid: &str) -> Result<(), StoreError> {
        self.lock()
            .signing_keys
            .remove(&(realm_id, kid.to_string()))
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }

    async fn try_acquire_rotation_lock(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut inner = self.lock();
        let held_by_other = inner
            .rotation_lock
            .as_ref()
            .is_some_and(|l| l.holder != holder && l.expires_at > now);
        if held_by_other {
            return Ok(false);
        }
        inner.rotation_lock = Some(RotationLock {
            holder: holder.to_string(),
            expires_at,
        });
        Ok(true)
    }

    async fn release_rotation_lock(&self, holder: &str) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner
            .rotation_lock
            .as_ref()
            .is_some_and(|l| l.holder == holder)
        {
            inner.rotation_lock = None;
        }
        Ok(())
    }

    // ---------------- Flow records ----------------

    async fn put_auth_code(&self, code: AuthCode) -> Result<(), StoreError> {
        self.lock().auth_codes.insert(code.code.clone(), code);
        Ok(())
    }

    async fn take_auth_code(&self, code: &str) -> Result<AuthCode, StoreError> {
        self.lock()
            .auth_codes
            .remove(code)
            .ok_or(StoreError::NotFound)
    }

    async fn put_refresh_token(&self, token: RefreshToken) -> Result<(), StoreError> {
        self.lock().refresh_tokens.insert(token.jti, token);
        Ok(())
    }

    async fn get_refresh_token(&self, jti: Uuid) -> Result<RefreshToken, StoreError> {
        self.lock()
            .refresh_tokens
            .get(&jti)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn revoke_refresh_token(&self, jti: Uuid) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let t = inner
            .refresh_tokens
            .get_mut(&jti)
            .ok_or(StoreError::NotFound)?;
        t.revoked = true;
        Ok(())
    }

    async fn revoke_refresh_family(&self, family_id: Uuid) -> Result<(), StoreError> {
        let mut inner = self.lock();
        for t in inner.refresh_tokens.values_mut() {
            if t.family_id == family_id {
                t.revoked = true;
            }
        }
        Ok(())
    }

    async fn put_device_code(&self, dc: DeviceCode) -> Result<(), StoreError> {
        self.lock().device_codes.insert(dc.device_code.clone(), dc);
        Ok(())
    }

    async fn get_device_code_by_dc(&self, device_code: &str) -> Result<DeviceCode, StoreError> {
        self.lock()
            .device_codes
            .get(device_code)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_device_code_by_uc(&self, user_code: &str) -> Result<DeviceCode, StoreError> {
        self.lock()
            .device_codes
            .values()
            .find(|d| d.user_code == user_code)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn update_device_code_status(
        &self,
        device_code: &str,
        status: DeviceCodeStatus,
        user_id: Option<Uuid>,
        granted_tenant: Option<Uuid>,
    ) -> Result<DeviceCode, StoreError> {
        let mut inner = self.lock();
        let d = inner
            .device_codes
            .get_mut(device_code)
            .ok_or(StoreError::NotFound)?;
        d.status = status;
        if user_id.is_some() {
            d.user_id = user_id;
        }
        if granted_tenant.is_some() {
            d.granted_tenant = granted_tenant;
        }
        Ok(d.clone())
    }

    async fn put_session(&self, session: Session) -> Result<(), StoreError> {
        self.lock().sessions.insert(session.id, session);
        Ok(())
    }

    async fn get_session(&self, id: Uuid) -> Result<Session, StoreError> {
        self.lock()
            .sessions
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn delete_session(&self, id: Uuid) -> Result<(), StoreError> {
        self.lock()
            .sessions
            .remove(&id)
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }

    async fn put_broker_state(&self, st: BrokerState) -> Result<(), StoreError> {
        self.lock().broker_states.insert(st.state.clone(), st);
        Ok(())
    }

    async fn take_broker_state(&self, state: &str) -> Result<BrokerState, StoreError> {
        self.lock()
            .broker_states
            .remove(state)
            .ok_or(StoreError::NotFound)
    }

    async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<usize, StoreError> {
        let mut inner = self.lock();
        let mut removed = 0usize;
        macro_rules! sweep {
            ($map:expr) => {{
                let before = $map.len();
                $map.retain(|_, v| v.expires_at >= now);
                removed += before - $map.len();
            }};
        }
        sweep!(inner.auth_codes);
        sweep!(inner.refresh_tokens);
        sweep!(inner.device_codes);
        sweep!(inner.sessions);
        sweep!(inner.broker_states);
        Ok(removed)
    }
}
