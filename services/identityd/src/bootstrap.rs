// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Zero-config startup seeding.
//!
//! On boot the provider seeds a fixed system realm, a fixed tenant
//! realm (each with an Active signing key carrying the dev public JWK),
//! the demo user with a bcrypt-hashed password, a `TenantMember` role
//! assignment, and the Workbench OAuth client. All identifiers are the
//! pinned wire-contract constants in [`crate::identifiers`], so a fresh
//! MemStore comes up immediately usable by the BFF.

use chrono::{Duration, Utc};
use identity_store::{
    GrantType, IdentityStore, KeyStatus, NewOAuthClient, NewRealm, NewRoleAssignment, NewSigningKey,
    NewUser, RealmScope, Role, SigningAlg,
};
use identity_store::types::{AssignmentSubject, AssignmentTarget, RedactedString};

use crate::identifiers;

/// Seed both realms and the demo principal/client into `store`.
///
/// `public_jwk` is the JWK derived from the embedded signing key; it is
/// what every realm's JWKS endpoint publishes.
pub async fn seed<S: IdentityStore>(
    store: &S,
    public_jwk: serde_json::Value,
) -> anyhow::Result<()> {
    let now = Utc::now();

    // One Active signing key per realm; both reuse the single dev key.
    let signing_key = |realm_label: &str| NewSigningKey {
        kid: identifiers::SIGNING_KID.to_string(),
        alg: SigningAlg::Rs256,
        // The private PEM is held in-process (crate::keys); the store
        // copy is unused by this minimal provider, so we store a marker
        // rather than the real material.
        private_pem: RedactedString::from(format!("embedded-dev-key:{realm_label}")),
        public_jwk: public_jwk.clone(),
        status: KeyStatus::Active,
        not_before: now,
        not_after: now + Duration::days(3650),
    };

    // --- System realm (fleet operators). Singleton. ---
    store
        .create_realm(
            NewRealm {
                scope: RealmScope::System,
                name: "system".to_string(),
                description: Some("Fleet operator directory".to_string()),
                issuer_url: identifiers::realm_issuer_url(identifiers::SYSTEM_REALM_ID),
                signing_alg: Some(SigningAlg::Rs256),
                access_token_ttl_secs: Some(identifiers::ACCESS_TTL_SECS as u32),
                id_token_ttl_secs: Some(identifiers::ACCESS_TTL_SECS as u32),
                refresh_token_ttl_secs: Some(identifiers::REFRESH_TTL_SECS as u32),
                auth_code_ttl_secs: None,
                device_code_ttl_secs: None,
                login_policy: None,
            },
            vec![signing_key("system")],
        )
        .await
        .map_err(|e| anyhow::anyhow!("seed system realm: {e}"))?;

    // The MemStore assigns its own realm ids, but the wire contract
    // pins them. We keep the store untouched and route by the pinned
    // issuer URL instead (see `server::Ctx::resolve_realm`), so every
    // token's `iss`/`realm` claim and every `/realms/{realm}/...` path
    // still uses the contract ids.

    // --- Tenant realm. ---
    store
        .create_realm(
            NewRealm {
                scope: RealmScope::Tenant {
                    tenant_id: identifiers::TENANT_ID,
                },
                name: "mnx-internal".to_string(),
                description: Some("Workbench demo tenant realm".to_string()),
                issuer_url: identifiers::tenant_issuer_url(),
                signing_alg: Some(SigningAlg::Rs256),
                access_token_ttl_secs: Some(identifiers::ACCESS_TTL_SECS as u32),
                id_token_ttl_secs: Some(identifiers::ACCESS_TTL_SECS as u32),
                refresh_token_ttl_secs: Some(identifiers::REFRESH_TTL_SECS as u32),
                auth_code_ttl_secs: None,
                device_code_ttl_secs: None,
                login_policy: None,
            },
            vec![signing_key("tenant")],
        )
        .await
        .map_err(|e| anyhow::anyhow!("seed tenant realm: {e}"))?;

    // Resolve the tenant realm's store-assigned id (the MemStore picks
    // it) so the demo user, client, and role assignment attach to the
    // right realm.
    let tenant_realm = store
        .get_realm_by_issuer(&identifiers::tenant_issuer_url())
        .await
        .map_err(|e| anyhow::anyhow!("resolve tenant realm: {e}"))?;

    // --- Demo user (bcrypt-hashed password). ---
    let password_hash = bcrypt::hash(identifiers::DEMO_PASSWORD, bcrypt::DEFAULT_COST)
        .map_err(|e| anyhow::anyhow!("hash demo password: {e}"))?;
    let user = store
        .create_user(NewUser {
            realm_id: tenant_realm.id,
            username: identifiers::DEMO_USERNAME.to_string(),
            email: Some(identifiers::DEMO_EMAIL.to_string()),
            display_name: Some(identifiers::DEMO_DISPLAY_NAME.to_string()),
            password_hash,
            is_root: false,
            fleet_admin: false,
            brokered: None,
        })
        .await
        .map_err(|e| anyhow::anyhow!("seed demo user: {e}"))?;

    // --- Role assignment: user -> tenant, TenantMember. ---
    store
        .create_role_assignment(NewRoleAssignment {
            realm_id: tenant_realm.id,
            subject: AssignmentSubject::User { user_id: user.id },
            target: AssignmentTarget::Tenant {
                tenant_id: identifiers::TENANT_ID,
            },
            role: Role::TenantMember,
            created_by: user.id,
        })
        .await
        .map_err(|e| anyhow::anyhow!("seed role assignment: {e}"))?;

    // --- Workbench OAuth client (confidential). ---
    let client_secret_hash = bcrypt::hash(identifiers::CLIENT_SECRET, bcrypt::DEFAULT_COST)
        .map_err(|e| anyhow::anyhow!("hash client secret: {e}"))?;
    store
        .create_oauth_client(NewOAuthClient {
            realm_id: tenant_realm.id,
            name: identifiers::CLIENT_ID.to_string(),
            client_secret_hash: Some(client_secret_hash),
            redirect_uris: vec![],
            grant_types: vec![
                GrantType::AuthorizationCode,
                GrantType::RefreshToken,
                GrantType::ClientCredentials,
                // Dev/demo convenience: the zero-config Workbench flow
                // logs in via the password grant.
                GrantType::Password,
            ],
            pkce_required: false,
            scopes_allowed: vec!["openid".to_string(), "profile".to_string()],
            is_workload: false,
            bound_to_cn: None,
        })
        .await
        .map_err(|e| anyhow::anyhow!("seed oauth client: {e}"))?;

    Ok(())
}
