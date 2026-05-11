// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `IdentityStore` conformance suite.
//!
//! Every test below is parameterized over `impl IdentityStore + 'static`,
//! so the same scenarios run against every backend. The `MemStore` driver
//! lives in `tests/mem.rs`; once `FdbStore` lands a `tests/fdb.rs` driver
//! will call the same functions behind the `foundationdb` feature.
//!
//! Conventions:
//!
//! * Each `check_*` function takes an owned, freshly-constructed `S`. Tests
//!   never share state.
//! * Helpers (`assert_not_found`, `assert_conflict_msg`, fixtures) live at
//!   the top of this file.
//! * Wall-clock time is never used as an *invariant*. Methods that depend
//!   on time (`sweep_expired`, `try_acquire_rotation_lock`) take `now`
//!   explicitly; tests pass synthetic timestamps so the contract is
//!   deterministically testable on every backend.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use identity_store::types::*;
use identity_store::{IdentityStore, StoreError};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

/// Assert a `Result` is `Err(StoreError::NotFound)`. Anything else (including
/// a `Conflict` or an `Ok`) is a test failure with a helpful message.
pub fn assert_not_found<T: std::fmt::Debug>(r: Result<T, StoreError>) {
    match r {
        Err(StoreError::NotFound) => {}
        other => panic!("expected Err(NotFound), got {other:?}"),
    }
}

/// Assert a `Result` is `Err(StoreError::Conflict(msg))` whose message
/// contains `substring`. Substring assertions document *which* invariant we
/// expect to fire — refactors that conflate two rejections trip this.
pub fn assert_conflict_msg<T: std::fmt::Debug>(r: Result<T, StoreError>, substring: &str) {
    match r {
        Err(StoreError::Conflict(msg)) => {
            assert!(
                msg.contains(substring),
                "Conflict fired, but message {msg:?} didn't contain {substring:?}",
            );
        }
        other => panic!("expected Err(Conflict({substring:?})), got {other:?}"),
    }
}

/// Assert `Err(Conflict(_))` without pinning the message. Use sparingly —
/// prefer `assert_conflict_msg` when there is a security-relevant rule
/// you want to keep distinct from sibling rules.
pub fn assert_conflict<T: std::fmt::Debug>(r: Result<T, StoreError>) {
    match r {
        Err(StoreError::Conflict(_)) => {}
        other => panic!("expected Err(Conflict(_)), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A fixed timestamp the tests pin against, so any test that wants a stable
/// reference point (e.g. for `RefreshToken.expires_at`) can use offsets from
/// it. Using a constant rather than `Utc::now()` keeps tests deterministic.
pub fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

pub fn dummy_key(kid: &str, status: KeyStatus) -> NewSigningKey {
    NewSigningKey {
        kid: kid.to_string(),
        alg: SigningAlg::Rs256,
        private_pem: RedactedString::from(
            "-----BEGIN PRIVATE KEY-----\nDUMMY\n-----END PRIVATE KEY-----",
        ),
        public_jwk: serde_json::json!({ "kty": "RSA", "kid": kid, "n": "AQAB", "e": "AQAB" }),
        status,
        not_before: t0(),
        not_after: t0() + Duration::days(365),
    }
}

/// Seed two keys named `k-active` + `k-next`. Picked so kid-lex order is
/// (`k-active`, `k-next`) — useful for the ordering test.
pub fn ring() -> Vec<NewSigningKey> {
    vec![
        dummy_key("k-active", KeyStatus::Active),
        dummy_key("k-next", KeyStatus::Next),
    ]
}

pub fn new_realm(scope: RealmScope, issuer: &str) -> NewRealm {
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

pub async fn make_realm<S: IdentityStore>(store: &S, scope: RealmScope, issuer: &str) -> Realm {
    store
        .create_realm(new_realm(scope, issuer), ring())
        .await
        .expect("create realm")
}

pub async fn make_system_realm<S: IdentityStore>(store: &S) -> Realm {
    make_realm(
        store,
        RealmScope::System,
        "https://id.example/realms/system",
    )
    .await
}

pub async fn make_user<S: IdentityStore>(store: &S, realm_id: Uuid, username: &str) -> User {
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

pub async fn put_refresh<S: IdentityStore>(
    store: &S,
    realm_id: Uuid,
    user_id: Uuid,
    family_id: Uuid,
    expires_at: DateTime<Utc>,
) -> Uuid {
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
            expires_at,
            created_at: t0(),
        })
        .await
        .unwrap();
    jti
}

// ---------------------------------------------------------------------------
// Realm CRUD + seeded ring
// ---------------------------------------------------------------------------

pub async fn check_realm_round_trip_and_seeded_ring<S: IdentityStore>(store: S) {
    let realm = make_system_realm(&store).await;
    assert_eq!(realm.signing_alg, SigningAlg::Rs256);
    assert_eq!(realm.access_token_ttl_secs, DEFAULT_ACCESS_TOKEN_TTL_SECS);
    assert!(!realm.login_policy.mfa_required);
    assert!(realm.login_policy.password_login_allowed);

    // The seeded ring is exactly two keys (one Active, one Next), both with
    // the realm's signing_alg, in kid-lex order.
    let keys = store.list_signing_keys(realm.id).await.unwrap();
    assert_eq!(keys.len(), 2, "ring size");
    assert_eq!(
        keys.iter().map(|k| k.kid.as_str()).collect::<Vec<_>>(),
        vec!["k-active", "k-next"],
        "ring kid order is lex-sorted",
    );
    let by_status = |s: KeyStatus| keys.iter().filter(|k| k.status == s).count();
    assert_eq!(by_status(KeyStatus::Active), 1);
    assert_eq!(by_status(KeyStatus::Next), 1);
    assert!(keys.iter().all(|k| k.alg == realm.signing_alg));
    assert!(keys.iter().all(|k| k.realm_id == realm.id));

    // All three resolvers agree.
    assert_eq!(store.get_realm(realm.id).await.unwrap(), realm);
    assert_eq!(
        store.get_realm_by_issuer(&realm.issuer_url).await.unwrap(),
        realm,
    );
    assert_eq!(
        store.get_realm_by_scope(&RealmScope::System).await.unwrap(),
        realm,
    );
    assert_eq!(store.list_realms().await.unwrap().len(), 1);

    // Delete with no users/clients/connections succeeds; the ring is dropped
    // with it.
    store.delete_realm(realm.id).await.unwrap();
    assert_not_found(store.get_realm(realm.id).await);
    assert_not_found(store.list_signing_keys(realm.id).await);
}

pub async fn check_realm_uniqueness_rules<S: IdentityStore>(store: S) {
    let _ = make_system_realm(&store).await;

    // A second System realm fires the duplicate-scope rule.
    assert_conflict_msg(
        store
            .create_realm(
                new_realm(RealmScope::System, "https://id.example/realms/system2"),
                ring(),
            )
            .await,
        "scope",
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

    // Same tenant scope → duplicate-scope.
    assert_conflict_msg(
        store
            .create_realm(
                new_realm(
                    RealmScope::Tenant { tenant_id: t },
                    "https://id.example/realms/t-dup",
                ),
                ring(),
            )
            .await,
        "scope",
    );
    // Different scope, same issuer → duplicate-issuer (a *different* rule).
    assert_conflict_msg(
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
        "issuer",
    );
    // Empty ring → its own message; the realm must have at least one key
    // so JWKS isn't briefly empty after a successful create.
    assert_conflict_msg(
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
        "signing key",
    );
    // Duplicate kid inside the seed ring → its own message.
    assert_conflict_msg(
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
        "kid",
    );
}

pub async fn check_update_realm_settings_round_trip<S: IdentityStore>(store: S) {
    let realm = make_system_realm(&store).await;
    let original = realm.settings();

    // settings() then re-apply: no field changes (canary for a future field
    // being added to RealmSettings but missed in `settings()`).
    let updated = store
        .update_realm_settings(realm.id, original.clone())
        .await
        .unwrap();
    assert_eq!(updated.settings(), original);

    // A real change sticks.
    let bumped = RealmSettings {
        access_token_ttl_secs: 7777,
        id_token_ttl_secs: 8888,
        refresh_token_ttl_secs: 99_999,
        auth_code_ttl_secs: 42,
        device_code_ttl_secs: 1234,
        login_policy: LoginPolicy {
            mfa_required: true,
            password_login_allowed: false,
        },
    };
    let after = store
        .update_realm_settings(realm.id, bumped.clone())
        .await
        .unwrap();
    assert_eq!(after.settings(), bumped);
    // Re-read from the store, not the returned value.
    assert_eq!(store.get_realm(realm.id).await.unwrap().settings(), bumped);

    // Immutable fields untouched.
    let fresh = store.get_realm(realm.id).await.unwrap();
    assert_eq!(fresh.issuer_url, realm.issuer_url);
    assert_eq!(fresh.scope, realm.scope);
    assert_eq!(fresh.signing_alg, realm.signing_alg);

    assert_not_found(store.update_realm_settings(Uuid::new_v4(), bumped).await);
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

pub async fn check_user_round_trip_and_uniqueness<S: IdentityStore>(store: S) {
    let sys = make_system_realm(&store).await;
    let other = make_realm(
        &store,
        RealmScope::Silo {
            silo_id: Uuid::new_v4(),
        },
        "https://id.example/realms/s",
    )
    .await;

    let u = make_user(&store, sys.id, "alice").await;
    assert_eq!(u.display_name, "alice"); // defaults from username
    assert_eq!(u.status, UserStatus::Active);
    assert!(u.mfa.is_none());
    assert!(u.brokered.is_none());

    assert_eq!(store.get_user(u.id).await.unwrap(), u);
    assert_eq!(
        store.get_user_by_username(sys.id, "alice").await.unwrap(),
        u
    );
    assert!(store.has_any_user_in_realm(sys.id).await.unwrap());
    assert!(!store.has_any_user_in_realm(other.id).await.unwrap());

    // Same username, *different* realm — independent.
    let other_alice = make_user(&store, other.id, "alice").await;
    assert_ne!(other_alice.id, u.id);
    assert_eq!(
        store
            .get_user_by_username(other.id, "alice")
            .await
            .unwrap()
            .id,
        other_alice.id,
    );

    // Same username, same realm — Conflict naming the username.
    assert_conflict_msg(
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
        "username",
    );

    // Missing realm → NotFound (not Conflict).
    assert_not_found(
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
    );

    // Email uniqueness applies when set; two `None` emails coexist fine.
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
    assert_conflict_msg(
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
        "email",
    );
    // Two users with `None` email in the same realm are allowed.
    make_user(&store, sys.id, "dave").await;
    make_user(&store, sys.id, "erin").await;

    // delete_user is the lever that frees a username/email.
    store.delete_user(u.id).await.unwrap();
    assert_not_found(store.get_user(u.id).await);
    // Same name now reusable in the same realm.
    let _ = make_user(&store, sys.id, "alice").await;

    // Realm with users refuses to delete.
    assert_conflict(store.delete_realm(other.id).await);
}

pub async fn check_brokered_user_lookup<S: IdentityStore>(store: S) {
    let realm = make_realm(
        &store,
        RealmScope::Tenant {
            tenant_id: Uuid::new_v4(),
        },
        "https://id.example/realms/b",
    )
    .await;
    let conn_a = Uuid::new_v4();
    let conn_b = Uuid::new_v4();
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
                connection_id: conn_a,
                upstream_issuer: "https://okta.example".into(),
                upstream_subject: "abc".into(),
            }),
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .get_user_by_brokered(realm.id, conn_a, "abc")
            .await
            .unwrap(),
        u,
    );
    // Same subject under a *different* connection isn't the same user.
    assert_not_found(store.get_user_by_brokered(realm.id, conn_b, "abc").await);
    // Different subject under the same connection isn't either.
    assert_not_found(store.get_user_by_brokered(realm.id, conn_a, "other").await);
    // Different realm: also miss.
    let sys = make_system_realm(&store).await;
    assert_not_found(store.get_user_by_brokered(sys.id, conn_a, "abc").await);
}

pub async fn check_refresh_token_revocation<S: IdentityStore>(store: S) {
    let sys = make_system_realm(&store).await;
    let alice = make_user(&store, sys.id, "alice").await;
    let bob = make_user(&store, sys.id, "bob").await;

    // alice has two tokens in different families; bob has one untouched.
    let alice1 = put_refresh(
        &store,
        sys.id,
        alice.id,
        Uuid::new_v4(),
        t0() + Duration::days(1),
    )
    .await;
    let alice2 = put_refresh(
        &store,
        sys.id,
        alice.id,
        Uuid::new_v4(),
        t0() + Duration::days(1),
    )
    .await;
    let bob_jti = put_refresh(
        &store,
        sys.id,
        bob.id,
        Uuid::new_v4(),
        t0() + Duration::days(1),
    )
    .await;

    // Single-token revoke.
    assert!(!store.get_refresh_token(alice1).await.unwrap().revoked);
    store.revoke_refresh_token(alice1).await.unwrap();
    assert!(store.get_refresh_token(alice1).await.unwrap().revoked);
    // Sibling untouched.
    assert!(!store.get_refresh_token(alice2).await.unwrap().revoked);

    // Disabling a user revokes *every* token under them.
    store
        .set_user_status(alice.id, UserStatus::Disabled)
        .await
        .unwrap();
    assert!(store.get_refresh_token(alice1).await.unwrap().revoked);
    assert!(store.get_refresh_token(alice2).await.unwrap().revoked);
    // Other user's token is untouched.
    assert!(!store.get_refresh_token(bob_jti).await.unwrap().revoked);

    // Family-revoke covers exactly the right family (theft detection).
    let fam = Uuid::new_v4();
    let f1 = put_refresh(&store, sys.id, bob.id, fam, t0() + Duration::days(1)).await;
    let f2 = put_refresh(&store, sys.id, bob.id, fam, t0() + Duration::days(1)).await;
    let unrelated = put_refresh(
        &store,
        sys.id,
        bob.id,
        Uuid::new_v4(),
        t0() + Duration::days(1),
    )
    .await;
    store.revoke_refresh_family(fam).await.unwrap();
    assert!(store.get_refresh_token(f1).await.unwrap().revoked);
    assert!(store.get_refresh_token(f2).await.unwrap().revoked);
    assert!(!store.get_refresh_token(unrelated).await.unwrap().revoked);

    // revoke_refresh_token on an absent jti is NotFound (not silent).
    assert_not_found(store.revoke_refresh_token(Uuid::new_v4()).await);
}

pub async fn check_revoked_token_survives_sweep<S: IdentityStore>(store: S) {
    // A revoked-but-unexpired token MUST be readable so a reuse attempt is
    // *detected* (theft detection), not silently NotFound. The sweeper must
    // only drop based on `expires_at`, not on `revoked`.
    let sys = make_system_realm(&store).await;
    let u = make_user(&store, sys.id, "u").await;
    let jti = put_refresh(
        &store,
        sys.id,
        u.id,
        Uuid::new_v4(),
        t0() + Duration::days(1),
    )
    .await;
    store.revoke_refresh_token(jti).await.unwrap();

    let dropped = store.sweep_expired(t0()).await.unwrap();
    assert_eq!(dropped, 0, "sweep dropped a revoked-but-fresh token");
    let read = store.get_refresh_token(jti).await.unwrap();
    assert!(read.revoked);
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

pub async fn check_group_membership_both_directions<S: IdentityStore>(store: S) {
    let sys = make_system_realm(&store).await;
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

    // Duplicate group name in same realm → Conflict.
    assert_conflict_msg(
        store
            .create_group(NewGroup {
                realm_id: sys.id,
                name: "admins".into(),
                description: None,
            })
            .await,
        "group",
    );

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

    assert_not_found(store.add_group_member(Uuid::new_v4(), u1.id).await);
    assert_not_found(store.add_group_member(g.id, Uuid::new_v4()).await);

    // Group-typed role assignment cascades on delete_group.
    let _ = store
        .create_role_assignment(NewRoleAssignment {
            realm_id: sys.id,
            subject: AssignmentSubject::Group { group_id: g.id },
            target: AssignmentTarget::Fleet,
            role: Role::Operator,
            created_by: u2.id,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .list_assignments_of_subject(&AssignmentSubject::Group { group_id: g.id })
            .await
            .unwrap()
            .len(),
        1,
    );
    store.delete_group(g.id).await.unwrap();
    assert_not_found(store.list_group_members(g.id).await);
    assert!(
        store
            .list_assignments_of_subject(&AssignmentSubject::Group { group_id: g.id })
            .await
            .unwrap()
            .is_empty(),
        "delete_group should cascade group-typed role assignments",
    );
}

// ---------------------------------------------------------------------------
// Role assignments — D-Id-7
// ---------------------------------------------------------------------------

async fn try_grant<S: IdentityStore>(
    store: &S,
    realm_id: Uuid,
    target: AssignmentTarget,
    role: Role,
) -> Result<RoleAssignment, StoreError> {
    let u = make_user(store, realm_id, &format!("u-{}", Uuid::new_v4())).await;
    store
        .create_role_assignment(NewRoleAssignment {
            realm_id,
            subject: AssignmentSubject::User { user_id: u.id },
            target,
            role,
            created_by: u.id,
        })
        .await
}

pub async fn check_role_assignment_cross_scope_rejection<S: IdentityStore>(store: S) {
    let sys = make_system_realm(&store).await;
    let s1 = Uuid::new_v4();
    let s2 = Uuid::new_v4();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let tenant_realm = make_realm(
        &store,
        RealmScope::Tenant { tenant_id: t1 },
        "https://id.example/realms/tr",
    )
    .await;
    let silo_realm = make_realm(
        &store,
        RealmScope::Silo { silo_id: s1 },
        "https://id.example/realms/sr",
    )
    .await;

    // Vary the role for every test row so the structural-scope rule is shown
    // to be role-independent.
    let roles = [Role::TenantMember, Role::ReadOnly, Role::FleetAdmin];

    // System realm: Fleet only. Tenant/Silo targets → its own message.
    for role in roles {
        try_grant(&store, sys.id, AssignmentTarget::Fleet, role)
            .await
            .unwrap();
        assert_conflict_msg(
            try_grant(
                &store,
                sys.id,
                AssignmentTarget::Tenant { tenant_id: t1 },
                role,
            )
            .await,
            "System realm",
        );
        assert_conflict_msg(
            try_grant(&store, sys.id, AssignmentTarget::Silo { silo_id: s1 }, role).await,
            "System realm",
        );
    }

    // Tenant{t1}: only Tenant{t1}.
    for role in roles {
        try_grant(
            &store,
            tenant_realm.id,
            AssignmentTarget::Tenant { tenant_id: t1 },
            role,
        )
        .await
        .unwrap();
        assert_conflict_msg(
            try_grant(
                &store,
                tenant_realm.id,
                AssignmentTarget::Tenant { tenant_id: t2 },
                role,
            )
            .await,
            "tenant realm",
        );
        assert_conflict_msg(
            try_grant(
                &store,
                tenant_realm.id,
                AssignmentTarget::Silo { silo_id: s1 },
                role,
            )
            .await,
            "tenant realm",
        );
        assert_conflict_msg(
            try_grant(&store, tenant_realm.id, AssignmentTarget::Fleet, role).await,
            "only the System realm",
        );
    }

    // Silo{s1}: Silo{s1} or any Tenant; not Silo{s2} or Fleet.
    for role in roles {
        try_grant(
            &store,
            silo_realm.id,
            AssignmentTarget::Silo { silo_id: s1 },
            role,
        )
        .await
        .unwrap();
        try_grant(
            &store,
            silo_realm.id,
            AssignmentTarget::Tenant { tenant_id: t1 },
            role,
        )
        .await
        .unwrap();
        try_grant(
            &store,
            silo_realm.id,
            AssignmentTarget::Tenant { tenant_id: t2 },
            role,
        )
        .await
        .unwrap();
        assert_conflict_msg(
            try_grant(
                &store,
                silo_realm.id,
                AssignmentTarget::Silo { silo_id: s2 },
                role,
            )
            .await,
            "silo realm",
        );
        assert_conflict_msg(
            try_grant(&store, silo_realm.id, AssignmentTarget::Fleet, role).await,
            "only the System realm",
        );
    }
}

pub async fn check_role_assignment_round_trip_and_dup<S: IdentityStore>(store: S) {
    let sr = make_realm(
        &store,
        RealmScope::Silo {
            silo_id: Uuid::new_v4(),
        },
        "https://id.example/realms/x",
    )
    .await;
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
            .unwrap(),
        vec![a.clone()],
    );
    assert_eq!(
        store.list_assignments_for_target(&target).await.unwrap(),
        vec![a.clone()],
    );

    // Exact duplicate → Conflict.
    assert_conflict_msg(
        store
            .create_role_assignment(NewRoleAssignment {
                realm_id: sr.id,
                subject: AssignmentSubject::User { user_id: u.id },
                target: target.clone(),
                role: Role::TenantAdmin,
                created_by: u.id,
            })
            .await,
        "identical",
    );
    // Different *role* on the same subject+target → not a duplicate.
    let b = store
        .create_role_assignment(NewRoleAssignment {
            realm_id: sr.id,
            subject: AssignmentSubject::User { user_id: u.id },
            target: target.clone(),
            role: Role::ReadOnly,
            created_by: u.id,
        })
        .await
        .unwrap();
    let listed = store
        .list_assignments_of_subject(&AssignmentSubject::User { user_id: u.id })
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|x| x == &a));
    assert!(listed.iter().any(|x| x == &b));

    // Subject must exist in the realm.
    assert_not_found(
        store
            .create_role_assignment(NewRoleAssignment {
                realm_id: sr.id,
                subject: AssignmentSubject::User {
                    user_id: Uuid::new_v4(),
                },
                target,
                role: Role::ReadOnly,
                created_by: u.id,
            })
            .await,
    );

    store.delete_role_assignment(a.id).await.unwrap();
    assert_not_found(store.get_role_assignment(a.id).await);
    // delete_role_assignment of an absent id is NotFound, not silent.
    assert_not_found(store.delete_role_assignment(Uuid::new_v4()).await);
}

// ---------------------------------------------------------------------------
// OAuth clients
// ---------------------------------------------------------------------------

pub async fn check_oauth_client_round_trip_and_cn_binding<S: IdentityStore>(store: S) {
    let sys = make_system_realm(&store).await;
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
    assert_eq!(store.get_oauth_client_by_cn(cn).await.unwrap(), c);
    assert_eq!(
        store
            .list_oauth_clients_in_realm(sys.id)
            .await
            .unwrap()
            .len(),
        1,
    );

    // Second client for same CN → Conflict (per-CN binding is unique).
    assert_conflict_msg(
        store.create_oauth_client(mk(Some(cn))).await,
        "compute node",
    );
    // Unbound client → OK.
    let _ = store.create_oauth_client(mk(None)).await.unwrap();
    // Missing realm → NotFound.
    assert_not_found(
        store
            .create_oauth_client(NewOAuthClient {
                realm_id: Uuid::new_v4(),
                ..mk(None)
            })
            .await,
    );
    // get_oauth_client_by_cn on a CN with no client → NotFound.
    assert_not_found(store.get_oauth_client_by_cn(Uuid::new_v4()).await);

    // Update round-trip — every mutable field changes, immutable ones do not.
    let update = OAuthClientUpdate {
        name: "renamed".into(),
        client_secret_hash: None,
        redirect_uris: vec!["https://cb".into()],
        grant_types: vec![GrantType::ClientCredentials, GrantType::RefreshToken],
        pkce_required: true,
        scopes_allowed: vec!["triton:agent".into(), "triton:read".into()],
    };
    let after = store
        .update_oauth_client(c.id, update.clone())
        .await
        .unwrap();
    assert_eq!(after.name, update.name);
    assert_eq!(after.client_secret_hash, update.client_secret_hash);
    assert_eq!(after.redirect_uris, update.redirect_uris);
    assert_eq!(after.grant_types, update.grant_types);
    assert_eq!(after.pkce_required, update.pkce_required);
    assert_eq!(after.scopes_allowed, update.scopes_allowed);
    // Immutable.
    assert_eq!(after.id, c.id);
    assert_eq!(after.realm_id, c.realm_id);
    assert_eq!(after.is_workload, c.is_workload);
    assert_eq!(after.bound_to_cn, c.bound_to_cn);
    assert_eq!(after.created_at, c.created_at);
    // And it's actually persisted.
    assert_eq!(store.get_oauth_client(c.id).await.unwrap(), after);

    assert_not_found(
        store
            .update_oauth_client(Uuid::new_v4(), update.clone())
            .await,
    );
    store.delete_oauth_client(c.id).await.unwrap();
    assert_not_found(store.get_oauth_client(c.id).await);
    // Freeing the CN binding lets a new client claim it.
    let reissued = store.create_oauth_client(mk(Some(cn))).await.unwrap();
    assert_eq!(store.get_oauth_client_by_cn(cn).await.unwrap(), reissued);
}

// ---------------------------------------------------------------------------
// Upstream connections
// ---------------------------------------------------------------------------

pub async fn check_upstream_connection_and_mappings<S: IdentityStore>(store: S) {
    let realm = make_realm(
        &store,
        RealmScope::Tenant {
            tenant_id: Uuid::new_v4(),
        },
        "https://id.example/realms/conn",
    )
    .await;
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
        store.list_connections_in_realm(realm.id).await.unwrap(),
        vec![conn.clone()]
    );

    // Missing realm → NotFound on create.
    assert_not_found(
        store
            .create_upstream_connection(NewUpstreamConnection {
                realm_id: Uuid::new_v4(),
                name: "x".into(),
                kind: ConnectionKind::Oidc {
                    issuer_url: "https://x".into(),
                    client_id: "x".into(),
                    client_secret: RedactedString::from("x"),
                    scopes: vec![],
                    audience: None,
                },
                enabled: false,
            })
            .await,
    );

    // Duplicate name in same realm → Conflict (regardless of protocol).
    assert_conflict_msg(
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
        "connection",
    );

    // Mappings: put-replaces. Putting an empty list clears.
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
    store.put_claim_mappings(conn.id, vec![]).await.unwrap();
    assert!(store.list_claim_mappings(conn.id).await.unwrap().is_empty());
    assert_not_found(store.list_claim_mappings(Uuid::new_v4()).await);

    // set_connection_enabled flips a bit; the stored value matches.
    let toggled = store.set_connection_enabled(conn.id, false).await.unwrap();
    assert!(!toggled.enabled);
    assert!(
        !store
            .get_upstream_connection(conn.id)
            .await
            .unwrap()
            .enabled
    );

    // delete_upstream_connection cascades the mappings.
    store
        .put_claim_mappings(
            conn.id,
            vec![ClaimMapping {
                connection_id: conn.id,
                seq: 0,
                source: "x".into(),
                target: MappedField::Username,
                group_value: None,
            }],
        )
        .await
        .unwrap();
    store.delete_upstream_connection(conn.id).await.unwrap();
    assert_not_found(store.get_upstream_connection(conn.id).await);
    assert_not_found(store.list_claim_mappings(conn.id).await);
}

// ---------------------------------------------------------------------------
// Signing keys
// ---------------------------------------------------------------------------

pub async fn check_signing_keys_ring<S: IdentityStore>(store: S) {
    let realm = make_system_realm(&store).await;

    // Seed with non-sorted kids; ensure list returns them in lex order.
    let _ = store
        .add_signing_key(realm.id, dummy_key("kid-zebra", KeyStatus::Next))
        .await
        .unwrap();
    let _ = store
        .add_signing_key(realm.id, dummy_key("kid-alpha", KeyStatus::Retiring))
        .await
        .unwrap();

    let keys = store.list_signing_keys(realm.id).await.unwrap();
    assert_eq!(
        keys.iter().map(|k| k.kid.as_str()).collect::<Vec<_>>(),
        vec!["k-active", "k-next", "kid-alpha", "kid-zebra"],
        "list_signing_keys must be lex-sorted on kid",
    );

    // Duplicate kid → Conflict; missing realm → NotFound.
    assert_conflict_msg(
        store
            .add_signing_key(realm.id, dummy_key("kid-zebra", KeyStatus::Next))
            .await,
        "kid",
    );
    assert_not_found(
        store
            .add_signing_key(Uuid::new_v4(), dummy_key("orphan", KeyStatus::Next))
            .await,
    );

    // set_signing_key_status flips the field.
    let promoted = store
        .set_signing_key_status(realm.id, "kid-zebra", KeyStatus::Active)
        .await
        .unwrap();
    assert_eq!(promoted.status, KeyStatus::Active);
    // The store does NOT enforce "exactly one Active" — that policy lives
    // in identityd's rotation loop, not the store. Document this here.
    let actives = store
        .list_signing_keys(realm.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|k| k.status == KeyStatus::Active)
        .count();
    assert_eq!(actives, 2, "store does not enforce ring shape");

    assert_not_found(
        store
            .set_signing_key_status(realm.id, "nope", KeyStatus::Revoked)
            .await,
    );

    store
        .delete_signing_key(realm.id, "kid-zebra")
        .await
        .unwrap();
    assert_not_found(store.get_signing_key(realm.id, "kid-zebra").await);
    assert_not_found(store.delete_signing_key(realm.id, "kid-zebra").await);
}

// ---------------------------------------------------------------------------
// Flow records
// ---------------------------------------------------------------------------

pub async fn check_auth_code_and_broker_state_take_on_consumption<S: IdentityStore>(store: S) {
    let realm = make_system_realm(&store).await;
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
            expires_at: t0() + Duration::seconds(30),
        })
        .await
        .unwrap();
    assert_eq!(store.take_auth_code("AC").await.unwrap().code, "AC");
    assert_not_found(store.take_auth_code("AC").await); // single-use

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
            expires_at: t0() + Duration::seconds(120),
        })
        .await
        .unwrap();
    assert_eq!(store.take_broker_state("BS").await.unwrap().state, "BS");
    assert_not_found(store.take_broker_state("BS").await);
}

pub async fn check_device_code_lookup_and_status_update<S: IdentityStore>(store: S) {
    let realm = make_system_realm(&store).await;
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
            expires_at: t0() + Duration::seconds(600),
            created_at: t0(),
        })
        .await
        .unwrap();
    assert_eq!(
        store.get_device_code_by_dc("DC").await.unwrap().status,
        DeviceCodeStatus::Pending
    );
    assert_eq!(
        store
            .get_device_code_by_uc("WXYZ-1234")
            .await
            .unwrap()
            .device_code,
        "DC",
    );

    // Approve.
    let uid = Uuid::new_v4();
    let tid = Uuid::new_v4();
    let approved = store
        .update_device_code_status("DC", DeviceCodeStatus::Approved, Some(uid), Some(tid))
        .await
        .unwrap();
    assert_eq!(approved.status, DeviceCodeStatus::Approved);
    assert_eq!(approved.user_id, Some(uid));
    assert_eq!(approved.granted_tenant, Some(tid));

    // Pin the current contract: passing `None` for user_id / granted_tenant
    // means "leave unchanged", not "clear". This is the documented behaviour;
    // if we ever need to clear (e.g. deny-after-approve), the signature must
    // gain explicit `Clear`/`Set(...)` variants.
    let still_approved = store
        .update_device_code_status("DC", DeviceCodeStatus::Denied, None, None)
        .await
        .unwrap();
    assert_eq!(still_approved.status, DeviceCodeStatus::Denied);
    assert_eq!(
        still_approved.user_id,
        Some(uid),
        "user_id is preserved when None"
    );
    assert_eq!(
        still_approved.granted_tenant,
        Some(tid),
        "granted_tenant preserved when None"
    );

    assert_not_found(
        store
            .update_device_code_status("nope", DeviceCodeStatus::Approved, None, None)
            .await,
    );
}

pub async fn check_sweeper_boundary<S: IdentityStore>(store: S) {
    let realm = make_system_realm(&store).await;
    let u = make_user(&store, realm.id, "u").await;
    let cutoff = t0() + Duration::seconds(1000);

    // Three buckets relative to the cutoff: just before, exactly at, just after.
    let before = put_refresh(
        &store,
        realm.id,
        u.id,
        Uuid::new_v4(),
        cutoff - Duration::seconds(1),
    )
    .await;
    let exact = put_refresh(&store, realm.id, u.id, Uuid::new_v4(), cutoff).await;
    let after = put_refresh(
        &store,
        realm.id,
        u.id,
        Uuid::new_v4(),
        cutoff + Duration::seconds(1),
    )
    .await;

    let dropped = store.sweep_expired(cutoff).await.unwrap();
    assert_eq!(dropped, 1, "exactly the one with expires_at < cutoff");
    assert_not_found(store.get_refresh_token(before).await);
    // `== cutoff` is kept — the contract is "drop where expires_at < now".
    assert!(store.get_refresh_token(exact).await.is_ok());
    assert!(store.get_refresh_token(after).await.is_ok());
}

// ---------------------------------------------------------------------------
// Cross-cutting
// ---------------------------------------------------------------------------

pub async fn check_delete_realm_blocked_by_each_child<S: IdentityStore>(store: S) {
    // A realm with users → Conflict.
    {
        let r = make_realm(
            &store,
            RealmScope::Tenant {
                tenant_id: Uuid::new_v4(),
            },
            "https://id.example/realms/blocked-by-user",
        )
        .await;
        let _ = make_user(&store, r.id, "u").await;
        assert_conflict(store.delete_realm(r.id).await);
    }
    // A realm with an OAuth client → Conflict.
    {
        let r = make_realm(
            &store,
            RealmScope::Tenant {
                tenant_id: Uuid::new_v4(),
            },
            "https://id.example/realms/blocked-by-client",
        )
        .await;
        let _ = store
            .create_oauth_client(NewOAuthClient {
                realm_id: r.id,
                name: "svc".into(),
                client_secret_hash: Some("h".into()),
                redirect_uris: vec![],
                grant_types: vec![GrantType::ClientCredentials],
                pkce_required: false,
                scopes_allowed: vec![],
                is_workload: true,
                bound_to_cn: None,
            })
            .await
            .unwrap();
        assert_conflict(store.delete_realm(r.id).await);
    }
    // A realm with an upstream connection → Conflict.
    {
        let r = make_realm(
            &store,
            RealmScope::Tenant {
                tenant_id: Uuid::new_v4(),
            },
            "https://id.example/realms/blocked-by-conn",
        )
        .await;
        let _ = store
            .create_upstream_connection(NewUpstreamConnection {
                realm_id: r.id,
                name: "ext".into(),
                kind: ConnectionKind::Oidc {
                    issuer_url: "https://upstream".into(),
                    client_id: "x".into(),
                    client_secret: RedactedString::from("x"),
                    scopes: vec![],
                    audience: None,
                },
                enabled: true,
            })
            .await
            .unwrap();
        assert_conflict(store.delete_realm(r.id).await);
    }
}

pub async fn check_multi_realm_list_isolation<S: IdentityStore>(store: S) {
    let a = make_realm(
        &store,
        RealmScope::Tenant {
            tenant_id: Uuid::new_v4(),
        },
        "https://id.example/realms/A",
    )
    .await;
    let b = make_realm(
        &store,
        RealmScope::Tenant {
            tenant_id: Uuid::new_v4(),
        },
        "https://id.example/realms/B",
    )
    .await;
    let _ua = make_user(&store, a.id, "in-a").await;
    let _ub1 = make_user(&store, b.id, "in-b-1").await;
    let _ub2 = make_user(&store, b.id, "in-b-2").await;
    let _ga = store
        .create_group(NewGroup {
            realm_id: a.id,
            name: "g".into(),
            description: None,
        })
        .await
        .unwrap();
    let _ca = store
        .create_oauth_client(NewOAuthClient {
            realm_id: a.id,
            name: "ca".into(),
            client_secret_hash: Some("h".into()),
            redirect_uris: vec![],
            grant_types: vec![GrantType::ClientCredentials],
            pkce_required: false,
            scopes_allowed: vec![],
            is_workload: true,
            bound_to_cn: None,
        })
        .await
        .unwrap();
    let _cb1 = store
        .create_oauth_client(NewOAuthClient {
            realm_id: b.id,
            name: "cb1".into(),
            client_secret_hash: Some("h".into()),
            redirect_uris: vec![],
            grant_types: vec![GrantType::ClientCredentials],
            pkce_required: false,
            scopes_allowed: vec![],
            is_workload: true,
            bound_to_cn: None,
        })
        .await
        .unwrap();

    assert_eq!(store.list_users_in_realm(a.id).await.unwrap().len(), 1);
    assert_eq!(store.list_users_in_realm(b.id).await.unwrap().len(), 2);
    assert!(
        store
            .list_users_in_realm(a.id)
            .await
            .unwrap()
            .iter()
            .all(|u| u.realm_id == a.id),
    );
    assert_eq!(store.list_groups_in_realm(a.id).await.unwrap().len(), 1);
    assert_eq!(store.list_groups_in_realm(b.id).await.unwrap().len(), 0);
    assert_eq!(
        store.list_oauth_clients_in_realm(a.id).await.unwrap().len(),
        1
    );
    assert_eq!(
        store.list_oauth_clients_in_realm(b.id).await.unwrap().len(),
        1
    );
    assert!(
        store
            .list_oauth_clients_in_realm(a.id)
            .await
            .unwrap()
            .iter()
            .all(|c| c.realm_id == a.id),
    );
}

pub async fn check_key_encoding_hostile_inputs<S: IdentityStore>(store: S) {
    // Pin the current contract: the store treats names/kids/issuers as
    // opaque byte strings — it round-trips characters that would naively
    // collide with FDB key segments (slashes, NUL, empty). When FdbStore
    // lands, this test forces an explicit decision: either escape such
    // inputs at the FDB-key layer, or reject them at create time. *Today*
    // the contract is "accept and round-trip"; if FdbStore can't honour
    // it, change both impls and this test together.
    let weird_issuer = "https://idp.example/path//with/slashes?q=1&x=/";
    let realm = make_realm(
        &store,
        RealmScope::Tenant {
            tenant_id: Uuid::new_v4(),
        },
        weird_issuer,
    )
    .await;
    assert_eq!(realm.issuer_url, weird_issuer);
    assert_eq!(
        store.get_realm_by_issuer(weird_issuer).await.unwrap().id,
        realm.id,
    );

    // username and group name with weird chars.
    let u = store
        .create_user(NewUser {
            realm_id: realm.id,
            username: "weird/name with spaces".into(),
            email: None,
            display_name: None,
            password_hash: String::new(),
            is_root: false,
            fleet_admin: false,
            brokered: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .get_user_by_username(realm.id, "weird/name with spaces")
            .await
            .unwrap(),
        u,
    );

    // signing-key with a weird kid.
    let weird_kid = "kid/with/slashes#and?stuff";
    let added = store
        .add_signing_key(realm.id, dummy_key(weird_kid, KeyStatus::Retiring))
        .await
        .unwrap();
    assert_eq!(added.kid, weird_kid);
    assert_eq!(
        store
            .get_signing_key(realm.id, weird_kid)
            .await
            .unwrap()
            .kid,
        weird_kid,
    );
}

pub async fn check_concurrent_create_realm_exactly_one_wins<S: IdentityStore + 'static>(store: S) {
    // The contract says create_realm is atomic on issuer + scope. Multiple
    // concurrent calls with the same (scope, issuer) must produce exactly one
    // Ok and the rest Conflict. Against MemStore this is trivially the Mutex;
    // against FdbStore it's the transactional first-writer-wins. The test is
    // identical either way.
    let store = Arc::new(store);
    let issuer = "https://id.example/realms/race";
    let mut handles = Vec::new();
    for _ in 0..8 {
        let s = Arc::clone(&store);
        let req = new_realm(
            RealmScope::Tenant {
                tenant_id: Uuid::nil(),
            }, // same scope every time
            issuer,
        );
        handles.push(tokio::spawn(
            async move { s.create_realm(req, ring()).await },
        ));
    }
    let mut ok = 0;
    let mut conflict = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(_) => ok += 1,
            Err(StoreError::Conflict(_)) => conflict += 1,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(ok, 1, "exactly one create_realm should succeed");
    assert_eq!(conflict, 7);
}

pub async fn check_not_found_for_absent_ids<S: IdentityStore>(store: S) {
    assert_not_found(store.get_realm(Uuid::new_v4()).await);
    assert_not_found(store.get_user(Uuid::new_v4()).await);
    assert_not_found(store.get_group(Uuid::new_v4()).await);
    assert_not_found(store.get_role_assignment(Uuid::new_v4()).await);
    assert_not_found(store.get_oauth_client(Uuid::new_v4()).await);
    assert_not_found(store.get_oauth_client_by_cn(Uuid::new_v4()).await);
    assert_not_found(store.get_upstream_connection(Uuid::new_v4()).await);
    assert_not_found(store.get_signing_key(Uuid::new_v4(), "k").await);
    assert_not_found(store.delete_realm(Uuid::new_v4()).await);
    assert_not_found(store.delete_user(Uuid::new_v4()).await);
    assert_not_found(store.delete_group(Uuid::new_v4()).await);
    assert_not_found(store.delete_oauth_client(Uuid::new_v4()).await);
    assert_not_found(store.delete_upstream_connection(Uuid::new_v4()).await);
    assert_not_found(store.delete_signing_key(Uuid::new_v4(), "k").await);
}

// ---------------------------------------------------------------------------
// Rotation lock (time-injectable via parameters; deterministic on every backend)
// ---------------------------------------------------------------------------

pub async fn check_rotation_lock_expiry<S: IdentityStore>(store: S) {
    let now = t0();
    let expires = now + Duration::seconds(60);

    // a acquires until now+60s.
    assert!(
        store
            .try_acquire_rotation_lock("a", now, expires)
            .await
            .unwrap()
    );
    // b at now+30s — a's lock is still valid, so deny.
    assert!(
        !store
            .try_acquire_rotation_lock(
                "b",
                now + Duration::seconds(30),
                now + Duration::seconds(90)
            )
            .await
            .unwrap(),
    );
    // a re-entrant for itself at now+30s — always succeeds (and may extend).
    assert!(
        store
            .try_acquire_rotation_lock(
                "a",
                now + Duration::seconds(30),
                now + Duration::seconds(90)
            )
            .await
            .unwrap(),
    );
    // After a's lock expires, b can acquire.
    assert!(
        store
            .try_acquire_rotation_lock(
                "b",
                now + Duration::seconds(91),
                now + Duration::seconds(151)
            )
            .await
            .unwrap(),
    );
    // Releasing as the wrong holder is a no-op.
    store.release_rotation_lock("a").await.unwrap();
    assert!(
        !store
            .try_acquire_rotation_lock(
                "c",
                now + Duration::seconds(100),
                now + Duration::seconds(160)
            )
            .await
            .unwrap(),
        "wrong-holder release must not free b's lock",
    );
    // Releasing as the right holder frees the lock for anyone.
    store.release_rotation_lock("b").await.unwrap();
    assert!(
        store
            .try_acquire_rotation_lock(
                "c",
                now + Duration::seconds(101),
                now + Duration::seconds(161)
            )
            .await
            .unwrap(),
    );
}
