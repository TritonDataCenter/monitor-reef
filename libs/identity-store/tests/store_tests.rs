// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `IdentityStore` behaviour tests, run against `MemStore`. This is the
//! slice IS-0 acceptance suite (see `rfd/00003/01-data-model-and-store.md`).

// This is a test crate; `allow-{unwrap,expect}-in-tests` only reaches
// `#[test]` fns, not the free helper fns below, so opt the whole file out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{Duration, Utc};
use identity_store::types::*;
use identity_store::{IdentityStore, MemStore, StoreError};
use uuid::Uuid;

fn dummy_key(kid: &str, status: KeyStatus) -> NewSigningKey {
    let now = Utc::now();
    NewSigningKey {
        kid: kid.to_string(),
        alg: SigningAlg::Rs256,
        private_pem: RedactedString::from(
            "-----BEGIN PRIVATE KEY-----\nDUMMY\n-----END PRIVATE KEY-----",
        ),
        public_jwk: serde_json::json!({ "kty": "RSA", "kid": kid, "n": "AQAB", "e": "AQAB" }),
        status,
        not_before: now,
        not_after: now + Duration::days(365),
    }
}

fn ring() -> Vec<NewSigningKey> {
    vec![
        dummy_key("k-active", KeyStatus::Active),
        dummy_key("k-next", KeyStatus::Next),
    ]
}

fn new_realm(scope: RealmScope, issuer: &str) -> NewRealm {
    NewRealm {
        scope,
        name: "r".into(),
        description: None,
        issuer_url: issuer.into(),
        signing_alg: None,
        access_token_ttl_secs: None,
        id_token_ttl_secs: None,
        refresh_token_ttl_secs: None,
        auth_code_ttl_secs: None,
        device_code_ttl_secs: None,
        login_policy: None,
    }
}

async fn system_realm(store: &MemStore) -> Realm {
    store
        .create_realm(
            new_realm(RealmScope::System, "https://id.example/realms/system"),
            ring(),
        )
        .await
        .expect("create system realm")
}

fn matches(err: Result<impl std::fmt::Debug, StoreError>, want_not_found: bool) {
    match err {
        Err(StoreError::NotFound) => assert!(want_not_found, "got NotFound, wanted Conflict"),
        Err(StoreError::Conflict(_)) => assert!(!want_not_found, "got Conflict, wanted NotFound"),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn realm_round_trip_and_seeded_ring() {
    let store = MemStore::new();
    let realm = system_realm(&store).await;
    assert_eq!(realm.signing_alg, SigningAlg::Rs256);
    assert_eq!(realm.access_token_ttl_secs, DEFAULT_ACCESS_TOKEN_TTL_SECS);

    // Acceptance #4: exactly two keys, one Active + one Next, with the realm's alg.
    let keys = store.list_signing_keys(realm.id).await.unwrap();
    assert_eq!(keys.len(), 2);
    let actives: Vec<_> = keys
        .iter()
        .filter(|k| k.status == KeyStatus::Active)
        .collect();
    let nexts: Vec<_> = keys
        .iter()
        .filter(|k| k.status == KeyStatus::Next)
        .collect();
    assert_eq!(actives.len(), 1);
    assert_eq!(nexts.len(), 1);
    assert!(keys.iter().all(|k| k.alg == SigningAlg::Rs256));

    assert_eq!(store.get_realm(realm.id).await.unwrap(), realm);
    assert_eq!(
        store
            .get_realm_by_issuer(&realm.issuer_url)
            .await
            .unwrap()
            .id,
        realm.id
    );
    assert_eq!(
        store
            .get_realm_by_scope(&RealmScope::System)
            .await
            .unwrap()
            .id,
        realm.id
    );
    assert_eq!(store.list_realms().await.unwrap().len(), 1);

    store.delete_realm(realm.id).await.unwrap();
    matches(store.get_realm(realm.id).await, true);
    assert!(store.list_signing_keys(realm.id).await.is_err());
}

#[tokio::test]
async fn realm_uniqueness() {
    let store = MemStore::new();
    let _ = system_realm(&store).await;

    // Second System realm → Conflict.
    matches(
        store
            .create_realm(
                new_realm(RealmScope::System, "https://id.example/realms/system2"),
                ring(),
            )
            .await,
        false,
    );

    let t = Uuid::new_v4();
    store
        .create_realm(
            new_realm(
                RealmScope::Tenant { tenant_id: t },
                "https://id.example/realms/t",
            ),
            ring(),
        )
        .await
        .unwrap();
    // Duplicate scope (same tenant) → Conflict.
    matches(
        store
            .create_realm(
                new_realm(
                    RealmScope::Tenant { tenant_id: t },
                    "https://id.example/realms/t-dup",
                ),
                ring(),
            )
            .await,
        false,
    );
    // Duplicate issuer (different scope) → Conflict.
    matches(
        store
            .create_realm(
                new_realm(
                    RealmScope::Silo {
                        silo_id: Uuid::new_v4(),
                    },
                    "https://id.example/realms/t",
                ),
                ring(),
            )
            .await,
        false,
    );
    // Empty ring → Conflict.
    matches(
        store
            .create_realm(
                new_realm(
                    RealmScope::Silo {
                        silo_id: Uuid::new_v4(),
                    },
                    "https://id.example/realms/empty",
                ),
                vec![],
            )
            .await,
        false,
    );
    // Duplicate kid in the seed ring → Conflict.
    matches(
        store
            .create_realm(
                new_realm(
                    RealmScope::Silo {
                        silo_id: Uuid::new_v4(),
                    },
                    "https://id.example/realms/dupkid",
                ),
                vec![
                    dummy_key("same", KeyStatus::Active),
                    dummy_key("same", KeyStatus::Next),
                ],
            )
            .await,
        false,
    );
}

/// Create a fresh user in `realm_id` and try to grant it `target` (role
/// is immaterial to the structural-scope check).
async fn try_grant(
    store: &MemStore,
    realm_id: Uuid,
    target: AssignmentTarget,
) -> Result<RoleAssignment, StoreError> {
    let u = make_user(store, realm_id, &format!("u-{}", Uuid::new_v4())).await;
    store
        .create_role_assignment(NewRoleAssignment {
            realm_id,
            subject: AssignmentSubject::User { user_id: u.id },
            target,
            role: Role::TenantMember,
            created_by: u.id,
        })
        .await
}

async fn make_user(store: &MemStore, realm_id: Uuid, username: &str) -> User {
    store
        .create_user(NewUser {
            realm_id,
            username: username.into(),
            email: None,
            display_name: None,
            password_hash: "bcrypt$dummy".into(),
            is_root: false,
            fleet_admin: false,
            brokered: None,
        })
        .await
        .expect("create user")
}

#[tokio::test]
async fn user_round_trip_and_uniqueness() {
    let store = MemStore::new();
    let sys = system_realm(&store).await;
    let other = store
        .create_realm(
            new_realm(
                RealmScope::Silo {
                    silo_id: Uuid::new_v4(),
                },
                "https://id.example/realms/s",
            ),
            ring(),
        )
        .await
        .unwrap();

    let u = make_user(&store, sys.id, "alice").await;
    assert_eq!(u.display_name, "alice"); // defaults to username
    assert_eq!(store.get_user(u.id).await.unwrap(), u);
    assert_eq!(
        store
            .get_user_by_username(sys.id, "alice")
            .await
            .unwrap()
            .id,
        u.id
    );
    assert_eq!(store.list_users_in_realm(sys.id).await.unwrap().len(), 1);
    assert!(store.has_any_user_in_realm(sys.id).await.unwrap());
    assert!(!store.has_any_user_in_realm(other.id).await.unwrap());

    // Same username, different realm → OK.
    make_user(&store, other.id, "alice").await;
    // Same username, same realm → Conflict.
    matches(
        store
            .create_user(NewUser {
                realm_id: sys.id,
                username: "alice".into(),
                email: None,
                display_name: None,
                password_hash: String::new(),
                is_root: false,
                fleet_admin: false,
                brokered: None,
            })
            .await,
        false,
    );
    // Missing realm → NotFound.
    matches(
        store
            .create_user(NewUser {
                realm_id: Uuid::new_v4(),
                username: "bob".into(),
                email: None,
                display_name: None,
                password_hash: String::new(),
                is_root: false,
                fleet_admin: false,
                brokered: None,
            })
            .await,
        true,
    );

    // Email uniqueness within a realm.
    store
        .create_user(NewUser {
            realm_id: sys.id,
            username: "carol".into(),
            email: Some("carol@x".into()),
            display_name: None,
            password_hash: String::new(),
            is_root: false,
            fleet_admin: false,
            brokered: None,
        })
        .await
        .unwrap();
    matches(
        store
            .create_user(NewUser {
                realm_id: sys.id,
                username: "carol2".into(),
                email: Some("carol@x".into()),
                display_name: None,
                password_hash: String::new(),
                is_root: false,
                fleet_admin: false,
                brokered: None,
            })
            .await,
        false,
    );

    store.delete_user(u.id).await.unwrap();
    matches(store.get_user(u.id).await, true);
    // Realm can't be deleted while it has users.
    matches(store.delete_realm(other.id).await, false);
}

#[tokio::test]
async fn brokered_user_lookup() {
    let store = MemStore::new();
    let realm = store
        .create_realm(
            new_realm(
                RealmScope::Tenant {
                    tenant_id: Uuid::new_v4(),
                },
                "https://id.example/realms/b",
            ),
            ring(),
        )
        .await
        .unwrap();
    let conn = Uuid::new_v4();
    let u = store
        .create_user(NewUser {
            realm_id: realm.id,
            username: "okta|abc".into(),
            email: None,
            display_name: Some("From Okta".into()),
            password_hash: String::new(),
            is_root: false,
            fleet_admin: false,
            brokered: Some(BrokeredLink {
                connection_id: conn,
                upstream_issuer: "https://okta.example".into(),
                upstream_subject: "abc".into(),
            }),
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .get_user_by_brokered(realm.id, conn, "abc")
            .await
            .unwrap()
            .id,
        u.id
    );
    matches(
        store
            .get_user_by_brokered(realm.id, conn, "different")
            .await,
        true,
    );
}

async fn put_refresh(store: &MemStore, realm_id: Uuid, user_id: Uuid, family_id: Uuid) -> Uuid {
    let now = Utc::now();
    let jti = Uuid::new_v4();
    store
        .put_refresh_token(RefreshToken {
            jti,
            realm_id,
            client_id: Uuid::new_v4(),
            user_id,
            scope: "openid".into(),
            granted_tenant: None,
            family_id,
            revoked: false,
            expires_at: now + Duration::days(1),
            created_at: now,
        })
        .await
        .unwrap();
    jti
}

#[tokio::test]
async fn disabling_user_revokes_refresh_families() {
    let store = MemStore::new();
    let sys = system_realm(&store).await;
    let u = make_user(&store, sys.id, "svc").await;
    let other_user = make_user(&store, sys.id, "other").await;

    // Two of `u`'s tokens (different families) + one belonging to someone else.
    let j1 = put_refresh(&store, sys.id, u.id, Uuid::new_v4()).await;
    let j2 = put_refresh(&store, sys.id, u.id, Uuid::new_v4()).await;
    let j_other = put_refresh(&store, sys.id, other_user.id, Uuid::new_v4()).await;

    store
        .set_user_status(u.id, UserStatus::Disabled)
        .await
        .unwrap();
    assert!(store.get_refresh_token(j1).await.unwrap().revoked);
    assert!(store.get_refresh_token(j2).await.unwrap().revoked);
    assert!(!store.get_refresh_token(j_other).await.unwrap().revoked);

    // Explicit per-family revoke (theft-detection lever).
    let fam = Uuid::new_v4();
    let ja = put_refresh(&store, sys.id, other_user.id, fam).await;
    let jb = put_refresh(&store, sys.id, other_user.id, fam).await;
    store.revoke_refresh_family(fam).await.unwrap();
    assert!(store.get_refresh_token(ja).await.unwrap().revoked);
    assert!(store.get_refresh_token(jb).await.unwrap().revoked);
    // an unrelated token is untouched
    assert!(!store.get_refresh_token(j_other).await.unwrap().revoked);
}

#[tokio::test]
async fn group_membership_both_directions() {
    let store = MemStore::new();
    let sys = system_realm(&store).await;
    let g = store
        .create_group(NewGroup {
            realm_id: sys.id,
            name: "admins".into(),
            description: None,
        })
        .await
        .unwrap();
    let u1 = make_user(&store, sys.id, "u1").await;
    let u2 = make_user(&store, sys.id, "u2").await;

    store.add_group_member(g.id, u1.id).await.unwrap();
    store.add_group_member(g.id, u2.id).await.unwrap();
    store.add_group_member(g.id, u1.id).await.unwrap(); // idempotent

    let mut members = store.list_group_members(g.id).await.unwrap();
    members.sort();
    let mut want = vec![u1.id, u2.id];
    want.sort();
    assert_eq!(members, want);
    assert_eq!(store.list_groups_of_user(u1.id).await.unwrap(), vec![g.id]);

    store.remove_group_member(g.id, u1.id).await.unwrap();
    assert_eq!(store.list_group_members(g.id).await.unwrap(), vec![u2.id]);
    assert!(store.list_groups_of_user(u1.id).await.unwrap().is_empty());

    // missing group / user → NotFound
    matches(store.add_group_member(Uuid::new_v4(), u1.id).await, true);
    matches(store.add_group_member(g.id, Uuid::new_v4()).await, true);

    store.delete_group(g.id).await.unwrap();
    assert!(store.list_group_members(g.id).await.is_err());
}

#[tokio::test]
async fn role_assignment_cross_scope_rejection() {
    let store = MemStore::new();
    let sys = system_realm(&store).await;
    let s1 = Uuid::new_v4();
    let s2 = Uuid::new_v4();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let tenant_realm = store
        .create_realm(
            new_realm(
                RealmScope::Tenant { tenant_id: t1 },
                "https://id.example/realms/tr",
            ),
            ring(),
        )
        .await
        .unwrap();
    let silo_realm = store
        .create_realm(
            new_realm(
                RealmScope::Silo { silo_id: s1 },
                "https://id.example/realms/sr",
            ),
            ring(),
        )
        .await
        .unwrap();

    // System realm: only Fleet.
    assert!(
        try_grant(&store, sys.id, AssignmentTarget::Fleet)
            .await
            .is_ok()
    );
    matches(
        try_grant(&store, sys.id, AssignmentTarget::Tenant { tenant_id: t1 }).await,
        false,
    );
    matches(
        try_grant(&store, sys.id, AssignmentTarget::Silo { silo_id: s1 }).await,
        false,
    );

    // Tenant{t1} realm: only Tenant{t1}.
    assert!(
        try_grant(
            &store,
            tenant_realm.id,
            AssignmentTarget::Tenant { tenant_id: t1 }
        )
        .await
        .is_ok()
    );
    matches(
        try_grant(
            &store,
            tenant_realm.id,
            AssignmentTarget::Tenant { tenant_id: t2 },
        )
        .await,
        false,
    );
    matches(
        try_grant(
            &store,
            tenant_realm.id,
            AssignmentTarget::Silo { silo_id: s1 },
        )
        .await,
        false,
    );
    matches(
        try_grant(&store, tenant_realm.id, AssignmentTarget::Fleet).await,
        false,
    );

    // Silo{s1} realm: Silo{s1} or any Tenant; not Silo{s2} or Fleet.
    assert!(
        try_grant(
            &store,
            silo_realm.id,
            AssignmentTarget::Silo { silo_id: s1 }
        )
        .await
        .is_ok()
    );
    assert!(
        try_grant(
            &store,
            silo_realm.id,
            AssignmentTarget::Tenant { tenant_id: t1 }
        )
        .await
        .is_ok()
    );
    assert!(
        try_grant(
            &store,
            silo_realm.id,
            AssignmentTarget::Tenant { tenant_id: t2 }
        )
        .await
        .is_ok()
    );
    matches(
        try_grant(
            &store,
            silo_realm.id,
            AssignmentTarget::Silo { silo_id: s2 },
        )
        .await,
        false,
    );
    matches(
        try_grant(&store, silo_realm.id, AssignmentTarget::Fleet).await,
        false,
    );
}

#[tokio::test]
async fn role_assignment_round_trip_and_dup() {
    let store = MemStore::new();
    let sr = store
        .create_realm(
            new_realm(
                RealmScope::Silo {
                    silo_id: Uuid::new_v4(),
                },
                "https://id.example/realms/x",
            ),
            ring(),
        )
        .await
        .unwrap();
    let u = make_user(&store, sr.id, "grantee").await;
    let target = AssignmentTarget::Tenant {
        tenant_id: Uuid::new_v4(),
    };
    let a = store
        .create_role_assignment(NewRoleAssignment {
            realm_id: sr.id,
            subject: AssignmentSubject::User { user_id: u.id },
            target: target.clone(),
            role: Role::TenantAdmin,
            created_by: u.id,
        })
        .await
        .unwrap();
    assert_eq!(store.get_role_assignment(a.id).await.unwrap(), a);
    assert_eq!(
        store
            .list_assignments_of_subject(&AssignmentSubject::User { user_id: u.id })
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .list_assignments_for_target(&target)
            .await
            .unwrap()
            .len(),
        1
    );

    // exact duplicate → Conflict
    matches(
        store
            .create_role_assignment(NewRoleAssignment {
                realm_id: sr.id,
                subject: AssignmentSubject::User { user_id: u.id },
                target: target.clone(),
                role: Role::TenantAdmin,
                created_by: u.id,
            })
            .await,
        false,
    );
    // different role → OK
    assert!(
        store
            .create_role_assignment(NewRoleAssignment {
                realm_id: sr.id,
                subject: AssignmentSubject::User { user_id: u.id },
                target,
                role: Role::ReadOnly,
                created_by: u.id,
            })
            .await
            .is_ok()
    );

    // unknown subject → NotFound
    matches(
        store
            .create_role_assignment(NewRoleAssignment {
                realm_id: sr.id,
                subject: AssignmentSubject::User {
                    user_id: Uuid::new_v4(),
                },
                target: AssignmentTarget::Tenant {
                    tenant_id: Uuid::new_v4(),
                },
                role: Role::ReadOnly,
                created_by: u.id,
            })
            .await,
        true,
    );

    store.delete_role_assignment(a.id).await.unwrap();
    matches(store.get_role_assignment(a.id).await, true);
}

#[tokio::test]
async fn oauth_client_round_trip_and_cn_binding() {
    let store = MemStore::new();
    let sys = system_realm(&store).await;
    let mk = |bound: Option<Uuid>| NewOAuthClient {
        realm_id: sys.id,
        name: "c".into(),
        client_secret_hash: Some("bcrypt$dummy".into()),
        redirect_uris: vec![],
        grant_types: vec![GrantType::ClientCredentials],
        pkce_required: false,
        scopes_allowed: vec!["triton:agent".into()],
        is_workload: true,
        bound_to_cn: bound,
    };
    let cn = Uuid::new_v4();
    let c = store.create_oauth_client(mk(Some(cn))).await.unwrap();
    assert_eq!(store.get_oauth_client(c.id).await.unwrap(), c);
    assert_eq!(store.get_oauth_client_by_cn(cn).await.unwrap().id, c.id);
    assert_eq!(
        store
            .list_oauth_clients_in_realm(sys.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // second client for the same CN → Conflict
    matches(store.create_oauth_client(mk(Some(cn))).await, false);
    // unbound client → OK
    assert!(store.create_oauth_client(mk(None)).await.is_ok());
    // missing realm → NotFound
    matches(
        store
            .create_oauth_client(NewOAuthClient {
                realm_id: Uuid::new_v4(),
                ..mk(None)
            })
            .await,
        true,
    );

    let updated = store
        .update_oauth_client(
            c.id,
            OAuthClientUpdate {
                name: "renamed".into(),
                client_secret_hash: None,
                redirect_uris: vec!["https://cb".into()],
                grant_types: vec![GrantType::ClientCredentials, GrantType::RefreshToken],
                pkce_required: false,
                scopes_allowed: vec!["triton:agent".into()],
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "renamed");
    assert!(updated.client_secret_hash.is_none());

    store.delete_oauth_client(c.id).await.unwrap();
    matches(store.get_oauth_client(c.id).await, true);
}

#[tokio::test]
async fn upstream_connection_and_mappings() {
    let store = MemStore::new();
    let realm = store
        .create_realm(
            new_realm(
                RealmScope::Tenant {
                    tenant_id: Uuid::new_v4(),
                },
                "https://id.example/realms/conn",
            ),
            ring(),
        )
        .await
        .unwrap();
    let conn = store
        .create_upstream_connection(NewUpstreamConnection {
            realm_id: realm.id,
            name: "corp-okta".into(),
            kind: ConnectionKind::Oidc {
                issuer_url: "https://okta.example".into(),
                client_id: "abc".into(),
                client_secret: RedactedString::from("shh"),
                scopes: vec!["openid".into(), "email".into()],
                audience: None,
            },
            enabled: true,
        })
        .await
        .unwrap();
    assert_eq!(conn.kind.tag(), "oidc");
    assert_eq!(store.get_upstream_connection(conn.id).await.unwrap(), conn);
    assert_eq!(
        store
            .list_connections_in_realm(realm.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // duplicate name in realm → Conflict
    matches(
        store
            .create_upstream_connection(NewUpstreamConnection {
                realm_id: realm.id,
                name: "corp-okta".into(),
                kind: ConnectionKind::Saml {
                    idp_metadata: "https://idp/meta".into(),
                    sp_entity_id: "sp".into(),
                    sp_acs_url: "https://sp/acs".into(),
                    want_signed_assertions: true,
                },
                enabled: false,
            })
            .await,
        false,
    );

    store
        .put_claim_mappings(
            conn.id,
            vec![
                ClaimMapping {
                    connection_id: conn.id,
                    seq: 0,
                    source: "email".into(),
                    target: MappedField::Email,
                    group_value: None,
                },
                ClaimMapping {
                    connection_id: conn.id,
                    seq: 1,
                    source: "groups".into(),
                    target: MappedField::Group,
                    group_value: Some("admins".into()),
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(store.list_claim_mappings(conn.id).await.unwrap().len(), 2);
    matches(store.list_claim_mappings(Uuid::new_v4()).await, true);

    let toggled = store.set_connection_enabled(conn.id, false).await.unwrap();
    assert!(!toggled.enabled);

    store.delete_upstream_connection(conn.id).await.unwrap();
    matches(store.get_upstream_connection(conn.id).await, true);
    assert!(store.list_claim_mappings(conn.id).await.is_err());
}

#[tokio::test]
async fn signing_keys_add_status_rotation_lock() {
    let store = MemStore::new();
    let realm = system_realm(&store).await;
    let added = store
        .add_signing_key(realm.id, dummy_key("k3", KeyStatus::Next))
        .await
        .unwrap();
    assert_eq!(added.realm_id, realm.id);
    assert_eq!(store.list_signing_keys(realm.id).await.unwrap().len(), 3);
    // duplicate kid → Conflict
    matches(
        store
            .add_signing_key(realm.id, dummy_key("k3", KeyStatus::Next))
            .await,
        false,
    );
    // missing realm → NotFound
    matches(
        store
            .add_signing_key(Uuid::new_v4(), dummy_key("k4", KeyStatus::Next))
            .await,
        true,
    );

    let promoted = store
        .set_signing_key_status(realm.id, "k3", KeyStatus::Active)
        .await
        .unwrap();
    assert_eq!(promoted.status, KeyStatus::Active);
    matches(
        store
            .set_signing_key_status(realm.id, "nope", KeyStatus::Revoked)
            .await,
        true,
    );

    store.delete_signing_key(realm.id, "k3").await.unwrap();
    matches(store.get_signing_key(realm.id, "k3").await, true);

    // rotation lock
    assert!(store.try_acquire_rotation_lock("a", 60).await.unwrap());
    assert!(!store.try_acquire_rotation_lock("b", 60).await.unwrap());
    assert!(store.try_acquire_rotation_lock("a", 60).await.unwrap()); // re-entrant for the holder
    store.release_rotation_lock("a").await.unwrap();
    assert!(store.try_acquire_rotation_lock("b", 60).await.unwrap());
    // releasing as the wrong holder is a no-op
    store.release_rotation_lock("a").await.unwrap();
    assert!(!store.try_acquire_rotation_lock("c", 60).await.unwrap());
}

#[tokio::test]
async fn flow_records_and_sweeper() {
    let store = MemStore::new();
    let realm = system_realm(&store).await;
    let now = Utc::now();

    // auth code: delete-on-read
    store
        .put_auth_code(AuthCode {
            code: "AC".into(),
            realm_id: realm.id,
            client_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            redirect_uri: "https://cb".into(),
            pkce_challenge: Some("xyz".into()),
            scope: "openid".into(),
            granted_tenant: None,
            nonce: None,
            expires_at: now + Duration::seconds(30),
        })
        .await
        .unwrap();
    assert_eq!(store.take_auth_code("AC").await.unwrap().code, "AC");
    matches(store.take_auth_code("AC").await, true); // already consumed

    // device code: lookup by dc and uc, status update
    store
        .put_device_code(DeviceCode {
            device_code: "DC".into(),
            user_code: "WXYZ-1234".into(),
            realm_id: realm.id,
            client_id: Uuid::new_v4(),
            scope: "openid".into(),
            status: DeviceCodeStatus::Pending,
            user_id: None,
            granted_tenant: None,
            interval_secs: 5,
            expires_at: now + Duration::seconds(600),
            created_at: now,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .get_device_code_by_uc("WXYZ-1234")
            .await
            .unwrap()
            .device_code,
        "DC"
    );
    let uid = Uuid::new_v4();
    let approved = store
        .update_device_code_status("DC", DeviceCodeStatus::Approved, Some(uid), None)
        .await
        .unwrap();
    assert_eq!(approved.status, DeviceCodeStatus::Approved);
    assert_eq!(approved.user_id, Some(uid));

    // session
    let sid = Uuid::new_v4();
    store
        .put_session(Session {
            id: sid,
            realm_id: realm.id,
            user_id: uid,
            idp_session_index: None,
            created_at: now,
            expires_at: now + Duration::seconds(3600),
        })
        .await
        .unwrap();
    assert_eq!(store.get_session(sid).await.unwrap().id, sid);
    store.delete_session(sid).await.unwrap();
    matches(store.get_session(sid).await, true);

    // broker state: delete-on-read
    store
        .put_broker_state(BrokerState {
            state: "BS".into(),
            realm_id: realm.id,
            connection_id: Uuid::new_v4(),
            downstream_client_id: Uuid::new_v4(),
            downstream_redirect_uri: "https://cb".into(),
            downstream_pkce_challenge: None,
            downstream_nonce: None,
            downstream_state: Some("orig".into()),
            expires_at: now + Duration::seconds(120),
        })
        .await
        .unwrap();
    assert_eq!(store.take_broker_state("BS").await.unwrap().state, "BS");
    matches(store.take_broker_state("BS").await, true);

    // sweeper: an expired refresh token + an expired device code go; a fresh one stays.
    let stale = Uuid::new_v4();
    store
        .put_refresh_token(RefreshToken {
            jti: stale,
            realm_id: realm.id,
            client_id: Uuid::new_v4(),
            user_id: uid,
            scope: "openid".into(),
            granted_tenant: None,
            family_id: Uuid::new_v4(),
            revoked: false,
            expires_at: now - Duration::seconds(1),
            created_at: now - Duration::days(2),
        })
        .await
        .unwrap();
    let fresh = Uuid::new_v4();
    store
        .put_refresh_token(RefreshToken {
            jti: fresh,
            realm_id: realm.id,
            client_id: Uuid::new_v4(),
            user_id: uid,
            scope: "openid".into(),
            granted_tenant: None,
            family_id: Uuid::new_v4(),
            revoked: false,
            expires_at: now + Duration::days(1),
            created_at: now,
        })
        .await
        .unwrap();
    store
        .put_device_code(DeviceCode {
            device_code: "DC-stale".into(),
            user_code: "AAAA-0000".into(),
            realm_id: realm.id,
            client_id: Uuid::new_v4(),
            scope: "openid".into(),
            status: DeviceCodeStatus::Pending,
            user_id: None,
            granted_tenant: None,
            interval_secs: 5,
            expires_at: now - Duration::seconds(1),
            created_at: now - Duration::seconds(700),
        })
        .await
        .unwrap();
    let removed = store.sweep_expired(now).await.unwrap();
    assert_eq!(removed, 2); // the stale refresh token + the stale device code (the approved "DC" is still fresh)
    matches(store.get_refresh_token(stale).await, true);
    assert!(store.get_refresh_token(fresh).await.is_ok());
}

#[tokio::test]
async fn not_found_for_absent_ids() {
    let store = MemStore::new();
    matches(store.get_realm(Uuid::new_v4()).await, true);
    matches(store.get_user(Uuid::new_v4()).await, true);
    matches(store.get_group(Uuid::new_v4()).await, true);
    matches(store.get_role_assignment(Uuid::new_v4()).await, true);
    matches(store.get_oauth_client(Uuid::new_v4()).await, true);
    matches(store.get_oauth_client_by_cn(Uuid::new_v4()).await, true);
    matches(store.get_upstream_connection(Uuid::new_v4()).await, true);
    matches(store.get_signing_key(Uuid::new_v4(), "k").await, true);
    matches(store.delete_realm(Uuid::new_v4()).await, true);
}
