// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FoundationDB key builders for the identity keyspace.
//!
//! Single source of truth for every key shape the [`super::FdbStore`]
//! reads or writes. Every key lives under the `identity/…` prefix, which
//! is disjoint from `tritond-store`'s `triton/…`-and-friends keyspace so
//! the two services can share one FDB cluster.
//!
//! # Encoding rules
//!
//! * UUIDs are written in their canonical hyphenated lowercase form, which
//!   is fixed-length and contains no `/`, so they make safe path segments.
//! * Free-form text that participates in *uniqueness* (issuer URL,
//!   username, group/connection name, brokered subject) is hashed with
//!   SHA-256 before becoming a key segment. The hash is fixed-length and
//!   slash-free, so an attacker can't craft a value whose raw bytes
//!   collide with a path separator and forge a different key. Uniqueness
//!   only needs equality, not ordering, so the hash is the right tool.
//! * The signing-key `kid`, by contrast, must list back in lexicographic
//!   order (the JWKS publish order), so it is written as the *raw trailing
//!   segment* of its key after a fixed-length realm-uuid prefix. As the
//!   last bytes of the key it round-trips any byte (including `/`) and
//!   preserves byte-lex ordering on the original `kid`.
//!
//! Functions are `pub(super)` because callers outside the backend module
//! have no business hand-rolling keys.

use uuid::Uuid;

use crate::types::{AssignmentSubject, AssignmentTarget, RealmScope, Role};

/// Root prefix for the entire identity keyspace.
const ROOT: &str = "identity";

// ── Realms ────────────────────────────────────────────────────────────

pub(super) fn realm_by_id_key(id: Uuid) -> Vec<u8> {
    format!("{ROOT}/realm/by_id/{id}").into_bytes()
}

pub(super) fn realm_by_issuer_key(issuer: &str) -> Vec<u8> {
    format!("{ROOT}/realm/by_issuer/{}", sha256_hex(issuer.as_bytes())).into_bytes()
}

/// Index a realm by its scope. Each scope kind maps to a distinct, stable
/// segment so a second `System` realm (or a duplicate tenant/silo scope)
/// collides on this exact key.
pub(super) fn realm_by_scope_key(scope: &RealmScope) -> Vec<u8> {
    match scope {
        RealmScope::Tenant { tenant_id } => {
            format!("{ROOT}/realm/by_scope/tenant/{tenant_id}").into_bytes()
        }
        RealmScope::Silo { silo_id } => {
            format!("{ROOT}/realm/by_scope/silo/{silo_id}").into_bytes()
        }
        RealmScope::System => format!("{ROOT}/realm/by_scope/system").into_bytes(),
    }
}

pub(super) fn realm_all_key(id: Uuid) -> Vec<u8> {
    format!("{ROOT}/realm/all/{id}").into_bytes()
}

pub(super) fn realm_all_prefix() -> Vec<u8> {
    format!("{ROOT}/realm/all/").into_bytes()
}

// ── Users ─────────────────────────────────────────────────────────────

pub(super) fn user_by_id_key(id: Uuid) -> Vec<u8> {
    format!("{ROOT}/user/by_id/{id}").into_bytes()
}

pub(super) fn user_by_username_key(realm_id: Uuid, username: &str) -> Vec<u8> {
    format!(
        "{ROOT}/user/by_username/{realm_id}/{}",
        sha256_hex(username.as_bytes())
    )
    .into_bytes()
}

pub(super) fn user_by_email_key(realm_id: Uuid, email: &str) -> Vec<u8> {
    format!(
        "{ROOT}/user/by_email/{realm_id}/{}",
        sha256_hex(email.as_bytes())
    )
    .into_bytes()
}

pub(super) fn user_by_brokered_key(
    realm_id: Uuid,
    connection_id: Uuid,
    upstream_subject: &str,
) -> Vec<u8> {
    // SHA-256 of `connection_id\0subject` → fixed-length, slash-free.
    let mut buf = Vec::new();
    buf.extend_from_slice(connection_id.to_string().as_bytes());
    buf.push(0);
    buf.extend_from_slice(upstream_subject.as_bytes());
    format!(
        "{ROOT}/user/by_brokered/{realm_id}/{}",
        sha256_hex(&buf)
    )
    .into_bytes()
}

pub(super) fn user_in_realm_key(realm_id: Uuid, user_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/user/in_realm/{realm_id}/{user_id}").into_bytes()
}

pub(super) fn user_in_realm_prefix(realm_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/user/in_realm/{realm_id}/").into_bytes()
}

// ── Groups ────────────────────────────────────────────────────────────

pub(super) fn group_by_id_key(id: Uuid) -> Vec<u8> {
    format!("{ROOT}/group/by_id/{id}").into_bytes()
}

pub(super) fn group_by_name_key(realm_id: Uuid, name: &str) -> Vec<u8> {
    format!(
        "{ROOT}/group/by_name/{realm_id}/{}",
        sha256_hex(name.as_bytes())
    )
    .into_bytes()
}

pub(super) fn group_in_realm_key(realm_id: Uuid, group_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/group/in_realm/{realm_id}/{group_id}").into_bytes()
}

pub(super) fn group_in_realm_prefix(realm_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/group/in_realm/{realm_id}/").into_bytes()
}

/// Membership edge `group → user`. Scanned to list a group's members.
pub(super) fn group_member_key(group_id: Uuid, user_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/group_member/{group_id}/{user_id}").into_bytes()
}

pub(super) fn group_member_prefix(group_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/group_member/{group_id}/").into_bytes()
}

/// Reverse membership edge `user → group`. Scanned to list a user's
/// groups without walking every group.
pub(super) fn user_group_key(user_id: Uuid, group_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/user_group/{user_id}/{group_id}").into_bytes()
}

pub(super) fn user_group_prefix(user_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/user_group/{user_id}/").into_bytes()
}

// ── Role assignments ──────────────────────────────────────────────────

pub(super) fn role_by_id_key(id: Uuid) -> Vec<u8> {
    format!("{ROOT}/role/by_id/{id}").into_bytes()
}

fn subject_seg(subject: &AssignmentSubject) -> String {
    match subject {
        AssignmentSubject::User { user_id } => format!("user/{user_id}"),
        AssignmentSubject::Group { group_id } => format!("group/{group_id}"),
    }
}

fn target_seg(target: &AssignmentTarget) -> String {
    match target {
        AssignmentTarget::Tenant { tenant_id } => format!("tenant/{tenant_id}"),
        AssignmentTarget::Silo { silo_id } => format!("silo/{silo_id}"),
        AssignmentTarget::Fleet => "fleet".to_string(),
    }
}

fn role_seg(role: Role) -> &'static str {
    match role {
        Role::TenantAdmin => "tenant_admin",
        Role::TenantMember => "tenant_member",
        Role::SiloAdmin => "silo_admin",
        Role::FleetAdmin => "fleet_admin",
        Role::Operator => "operator",
        Role::ReadOnly => "read_only",
    }
}

pub(super) fn role_by_subject_key(subject: &AssignmentSubject, assignment_id: Uuid) -> Vec<u8> {
    format!(
        "{ROOT}/role/by_subject/{}/{assignment_id}",
        subject_seg(subject)
    )
    .into_bytes()
}

pub(super) fn role_by_subject_prefix(subject: &AssignmentSubject) -> Vec<u8> {
    format!("{ROOT}/role/by_subject/{}/", subject_seg(subject)).into_bytes()
}

pub(super) fn role_by_target_key(target: &AssignmentTarget, assignment_id: Uuid) -> Vec<u8> {
    format!(
        "{ROOT}/role/by_target/{}/{assignment_id}",
        target_seg(target)
    )
    .into_bytes()
}

pub(super) fn role_by_target_prefix(target: &AssignmentTarget) -> Vec<u8> {
    format!("{ROOT}/role/by_target/{}/", target_seg(target)).into_bytes()
}

/// Exact-tuple uniqueness key for `(realm, subject, target, role)`. A
/// duplicate `create_role_assignment` collides here.
pub(super) fn role_dup_key(
    realm_id: Uuid,
    subject: &AssignmentSubject,
    target: &AssignmentTarget,
    role: Role,
) -> Vec<u8> {
    format!(
        "{ROOT}/role/dup/{realm_id}/{}/{}/{}",
        subject_seg(subject),
        target_seg(target),
        role_seg(role)
    )
    .into_bytes()
}

// ── OAuth clients ─────────────────────────────────────────────────────

pub(super) fn client_by_id_key(id: Uuid) -> Vec<u8> {
    format!("{ROOT}/client/by_id/{id}").into_bytes()
}

pub(super) fn client_by_cn_key(server_uuid: Uuid) -> Vec<u8> {
    format!("{ROOT}/client/by_cn/{server_uuid}").into_bytes()
}

pub(super) fn client_in_realm_key(realm_id: Uuid, client_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/client/in_realm/{realm_id}/{client_id}").into_bytes()
}

pub(super) fn client_in_realm_prefix(realm_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/client/in_realm/{realm_id}/").into_bytes()
}

// ── Upstream connections + claim mappings ─────────────────────────────

pub(super) fn conn_by_id_key(id: Uuid) -> Vec<u8> {
    format!("{ROOT}/conn/by_id/{id}").into_bytes()
}

pub(super) fn conn_by_realm_name_key(realm_id: Uuid, name: &str) -> Vec<u8> {
    format!(
        "{ROOT}/conn/by_name/{realm_id}/{}",
        sha256_hex(name.as_bytes())
    )
    .into_bytes()
}

pub(super) fn conn_in_realm_key(realm_id: Uuid, conn_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/conn/in_realm/{realm_id}/{conn_id}").into_bytes()
}

pub(super) fn conn_in_realm_prefix(realm_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/conn/in_realm/{realm_id}/").into_bytes()
}

/// The whole claim-mapping list for a connection lives in one value
/// (the trait replaces it wholesale via `put_claim_mappings`).
pub(super) fn claim_mappings_key(conn_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/claim_map/{conn_id}").into_bytes()
}

// ── Signing keys ──────────────────────────────────────────────────────

/// A realm's signing key, keyed by `(realm, kid)`. The `kid` is the raw
/// trailing segment so the per-realm range scan yields keys in
/// lexicographic `kid` order and round-trips any byte in the `kid`.
pub(super) fn signing_key_key(realm_id: Uuid, kid: &str) -> Vec<u8> {
    let mut k = format!("{ROOT}/signkey/by_realm/{realm_id}/").into_bytes();
    k.extend_from_slice(kid.as_bytes());
    k
}

pub(super) fn signing_key_prefix(realm_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/signkey/by_realm/{realm_id}/").into_bytes()
}

// ── Rotation lock (singleton) ─────────────────────────────────────────

pub(super) fn rotation_lock_key() -> Vec<u8> {
    format!("{ROOT}/rotation_lock").into_bytes()
}

// ── Short-lived flow records ──────────────────────────────────────────

/// Authorization code, keyed by the opaque code string (hashed so an
/// arbitrary code can't escape the keyspace).
pub(super) fn auth_code_key(code: &str) -> Vec<u8> {
    format!("{ROOT}/auth_code/{}", sha256_hex(code.as_bytes())).into_bytes()
}

pub(super) fn auth_code_prefix() -> Vec<u8> {
    format!("{ROOT}/auth_code/").into_bytes()
}

pub(super) fn refresh_by_id_key(jti: Uuid) -> Vec<u8> {
    format!("{ROOT}/refresh/by_id/{jti}").into_bytes()
}

pub(super) fn refresh_by_id_prefix() -> Vec<u8> {
    format!("{ROOT}/refresh/by_id/").into_bytes()
}

/// Index `family → jti` so `revoke_refresh_family` finds members without
/// scanning every token.
pub(super) fn refresh_family_key(family_id: Uuid, jti: Uuid) -> Vec<u8> {
    format!("{ROOT}/refresh/by_family/{family_id}/{jti}").into_bytes()
}

pub(super) fn refresh_family_prefix(family_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/refresh/by_family/{family_id}/").into_bytes()
}

/// Index `user → jti` so disabling a user revokes its tokens without a
/// full scan.
pub(super) fn refresh_user_key(user_id: Uuid, jti: Uuid) -> Vec<u8> {
    format!("{ROOT}/refresh/by_user/{user_id}/{jti}").into_bytes()
}

pub(super) fn refresh_user_prefix(user_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/refresh/by_user/{user_id}/").into_bytes()
}

pub(super) fn device_code_by_dc_key(device_code: &str) -> Vec<u8> {
    format!(
        "{ROOT}/device/by_dc/{}",
        sha256_hex(device_code.as_bytes())
    )
    .into_bytes()
}

pub(super) fn device_code_by_dc_prefix() -> Vec<u8> {
    format!("{ROOT}/device/by_dc/").into_bytes()
}

pub(super) fn device_code_by_uc_key(user_code: &str) -> Vec<u8> {
    format!(
        "{ROOT}/device/by_uc/{}",
        sha256_hex(user_code.as_bytes())
    )
    .into_bytes()
}

pub(super) fn session_by_id_key(id: Uuid) -> Vec<u8> {
    format!("{ROOT}/session/by_id/{id}").into_bytes()
}

pub(super) fn session_by_id_prefix() -> Vec<u8> {
    format!("{ROOT}/session/by_id/").into_bytes()
}

/// Index `user → session` so `delete_user` revokes a user's sessions
/// without scanning the fleet-wide session keyspace (mirrors
/// [`refresh_user_key`]).
pub(super) fn session_by_user_key(user_id: Uuid, session_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/session/by_user/{user_id}/{session_id}").into_bytes()
}

pub(super) fn session_by_user_prefix(user_id: Uuid) -> Vec<u8> {
    format!("{ROOT}/session/by_user/{user_id}/").into_bytes()
}

pub(super) fn broker_state_key(state: &str) -> Vec<u8> {
    format!("{ROOT}/broker_state/{}", sha256_hex(state.as_bytes())).into_bytes()
}

pub(super) fn broker_state_prefix() -> Vec<u8> {
    format!("{ROOT}/broker_state/").into_bytes()
}

// ── Hashing ───────────────────────────────────────────────────────────

/// SHA-256 a byte string and render it lowercase hex.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    static HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest.iter() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).expect("test uuid")
    }

    #[test]
    fn every_key_is_under_the_identity_root() {
        let r = uuid("11111111-1111-1111-1111-111111111111");
        let u = uuid("22222222-2222-2222-2222-222222222222");
        let g = uuid("33333333-3333-3333-3333-333333333333");
        let keys: Vec<Vec<u8>> = vec![
            realm_by_id_key(r),
            realm_by_issuer_key("https://x/path//y?q=1"),
            realm_by_scope_key(&RealmScope::System),
            realm_all_key(r),
            user_by_id_key(u),
            user_by_username_key(r, "weird/name with spaces"),
            user_by_email_key(r, "a@b"),
            user_by_brokered_key(r, g, "subj/with/slash"),
            user_in_realm_key(r, u),
            group_by_id_key(g),
            group_by_name_key(r, "admins"),
            group_member_key(g, u),
            user_group_key(u, g),
            role_by_id_key(r),
            client_by_id_key(r),
            client_by_cn_key(u),
            conn_by_id_key(r),
            conn_by_realm_name_key(r, "corp-okta"),
            claim_mappings_key(r),
            signing_key_key(r, "kid/with/slashes#and?stuff"),
            rotation_lock_key(),
            auth_code_key("AC"),
            refresh_by_id_key(u),
            device_code_by_dc_key("DC"),
            session_by_id_key(u),
            broker_state_key("BS"),
        ];
        for k in keys {
            assert!(
                k.starts_with(b"identity/"),
                "key escaped the identity/ root: {:?}",
                String::from_utf8_lossy(&k),
            );
        }
    }

    #[test]
    fn scope_index_distinguishes_every_kind() {
        let a = realm_by_scope_key(&RealmScope::System);
        let b = realm_by_scope_key(&RealmScope::Tenant {
            tenant_id: uuid("11111111-1111-1111-1111-111111111111"),
        });
        let c = realm_by_scope_key(&RealmScope::Silo {
            silo_id: uuid("11111111-1111-1111-1111-111111111111"),
        });
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn signing_key_kid_is_raw_trailing_segment_and_lex_orders() {
        // The kid is the trailing bytes, so a per-realm scan sorts on the
        // original kid (the JWKS publish order the suite pins).
        let r = uuid("11111111-1111-1111-1111-111111111111");
        let pfx = signing_key_prefix(r);
        for kid in ["k-active", "k-next", "kid-alpha", "kid-zebra"] {
            assert!(signing_key_key(r, kid).starts_with(&pfx));
        }
        let mut keys: Vec<Vec<u8>> = ["kid-zebra", "k-active", "kid-alpha", "k-next"]
            .iter()
            .map(|k| signing_key_key(r, k))
            .collect();
        keys.sort();
        // Byte-lex on the full key == byte-lex on the kid suffix (shared
        // fixed-length prefix), so the scan yields kid-lex order.
        let suffixes: Vec<String> = keys
            .iter()
            .map(|k| String::from_utf8_lossy(&k[pfx.len()..]).into_owned())
            .collect();
        assert_eq!(suffixes, vec!["k-active", "k-next", "kid-alpha", "kid-zebra"]);
    }

    #[test]
    fn role_indices_separate_subject_target_and_dup() {
        let realm = uuid("11111111-1111-1111-1111-111111111111");
        let user = uuid("22222222-2222-2222-2222-222222222222");
        let aid = uuid("33333333-3333-3333-3333-333333333333");
        let subj = AssignmentSubject::User { user_id: user };
        let tgt = AssignmentTarget::Fleet;
        assert!(role_by_subject_key(&subj, aid).starts_with(&role_by_subject_prefix(&subj)));
        assert!(role_by_target_key(&tgt, aid).starts_with(&role_by_target_prefix(&tgt)));
        // The dup key for distinct roles on the same subject/target differs,
        // so two different-role grants coexist (the suite asserts this).
        let admin = role_dup_key(realm, &subj, &tgt, Role::TenantAdmin);
        let ro = role_dup_key(realm, &subj, &tgt, Role::ReadOnly);
        assert_ne!(admin, ro);
    }

    #[test]
    fn sha256_index_is_slash_free_and_stable() {
        let h = sha256_hex(b"https://idp.example/path//with/slashes?q=1&x=/");
        assert_eq!(h.len(), 64);
        assert!(!h.contains('/'));
        assert_eq!(h, sha256_hex(b"https://idp.example/path//with/slashes?q=1&x=/"));
    }
}
