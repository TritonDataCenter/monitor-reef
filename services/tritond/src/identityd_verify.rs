// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Verifying access tokens minted by an external `identityd` (RFD 00004
//! IS-3).
//!
//! `tritond` is a relying party: it links `identity-token` and, when an
//! `identityd_issuer_url` is configured, turns a bearer token into a
//! [`Principal::Operator`] by verifying the RS256 signature against the
//! realm's published JWKS — no store round-trip, because the claims are
//! denormalized. When no issuer is configured this module is never
//! constructed, so the existing JWT/OIDC/api-key paths are unchanged.

use std::time::Duration;

use identity_token::claims::RealmScope;
use identity_token::{AccessClaims, PollingJwksSource, Verifier, VerifierOptions};
use tritond_store::ApiKeyScope;

use crate::auth::Principal;

/// JWKS cache TTL. A request for a `kid` the cache hasn't seen forces a
/// single refresh regardless of this, so rotation is still picked up
/// promptly; this only bounds how long a *known* key is trusted before
/// a background re-fetch.
const JWKS_TTL: Duration = Duration::from_secs(300);

/// A relying-party verifier for one identityd realm issuer.
pub struct IdentitydVerifier {
    issuer: String,
    verifier: Verifier<PollingJwksSource>,
}

impl IdentitydVerifier {
    /// Build a verifier for `issuer_url` (the realm's `iss`), polling
    /// `{issuer_url}/jwks` for keys.
    #[must_use]
    pub fn new(issuer_url: impl Into<String>) -> Self {
        let issuer = issuer_url.into();
        let jwks_url = format!("{issuer}/jwks");
        let source = PollingJwksSource::new(jwks_url, JWKS_TTL);
        let verifier = Verifier::new(source, VerifierOptions::new(issuer.clone()));
        Self { issuer, verifier }
    }

    /// The issuer this verifier trusts.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// Verify `token` and map its claims to a principal. `Ok(None)` means
    /// the token is not one this verifier accepts (bad signature, wrong
    /// issuer, expired, malformed) — the caller should fall through to
    /// the next auth mechanism rather than fail the request.
    pub async fn authenticate(&self, token: &str) -> Option<Principal> {
        match self.verifier.verify(token).await {
            Ok(claims) => Some(principal_from_claims(claims)),
            Err(e) => {
                tracing::debug!(error = %e, "identityd token rejected; falling through");
                None
            }
        }
    }
}

/// Map verified [`AccessClaims`] onto a tritond [`Principal::Operator`].
///
/// The token carries the user's tenancy and operator flags directly, so
/// no store lookup is needed. Two security properties are enforced on the
/// way in:
///
/// * **Fleet flags require a System realm.** A tenant- or silo-realm
///   token must never confer `is_root`/`fleet_admin`, even if the claim
///   carries them — that would be a tenant→fleet-root escalation. We gate
///   both on `realm_scope == System` here, mirroring the identityd mint
///   site (defense in depth).
/// * **Least-privilege scope.** `Principal { scope: None }` is promoted
///   to full access by `auth.rs`, so an absent scope must NOT map to
///   `None`. We translate the OAuth `scope` string to the narrowest
///   [`ApiKeyScope`] that fits and default to `ReadOnly` when no
///   admin/full scope is present.
///
/// TODO(RFD 00021): per-resource-server `aud` enforcement. identityd
/// mints `aud: None` today, so there is nothing to bind against yet; once
/// resource-server audiences exist, require the tritond audience here.
fn principal_from_claims(claims: AccessClaims) -> Principal {
    let system_realm = matches!(claims.realm_scope, RealmScope::System);
    Principal::Operator {
        user_id: claims.sub,
        is_root: system_realm && claims.is_root,
        fleet_admin: system_realm && claims.fleet_admin,
        capabilities: Default::default(),
        tenant_id: claims.tenant_id,
        silo_id: claims.silo_id,
        scope: Some(scope_from_claims(&claims)),
        bound_cn: claims.cnf.as_ref().and_then(|c| c.cn),
    }
}

/// Translate the token's space-delimited OAuth `scope` into a tritond
/// [`ApiKeyScope`], picking least privilege. `Full` is granted only when
/// an explicit admin/full scope is present; everything else (including an
/// absent or unrecognized scope) maps to the read-only floor so a token
/// can never silently inherit write access.
fn scope_from_claims(claims: &AccessClaims) -> ApiKeyScope {
    // Explicit broad-access scopes. Kept narrow on purpose: only these
    // exact tokens unlock writes.
    const FULL_SCOPES: [&str; 3] = ["admin", "full", "triton:full"];
    if FULL_SCOPES.iter().any(|s| claims.has_scope(s)) {
        return ApiKeyScope::Full;
    }
    ApiKeyScope::ReadOnly
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn claims() -> AccessClaims {
        AccessClaims {
            sub: Uuid::from_u128(0x44),
            iss: "http://127.0.0.1:8090/realms/r".to_string(),
            aud: None,
            exp: 0,
            iat: 0,
            nbf: None,
            realm: Uuid::from_u128(0x11),
            realm_scope: RealmScope::Tenant,
            tenant_id: Some(Uuid::from_u128(0x22)),
            silo_id: Some(Uuid::from_u128(0x33)),
            is_root: false,
            fleet_admin: true,
            groups: vec![],
            scope: Some("openid".to_string()),
            cnf: None,
        }
    }

    #[test]
    fn claims_map_to_operator_with_tenancy() {
        let p = principal_from_claims(claims());
        match p {
            Principal::Operator {
                user_id,
                tenant_id,
                silo_id,
                fleet_admin,
                is_root,
                scope,
                bound_cn,
                ..
            } => {
                assert_eq!(user_id, Uuid::from_u128(0x44));
                assert_eq!(tenant_id, Some(Uuid::from_u128(0x22)));
                assert_eq!(silo_id, Some(Uuid::from_u128(0x33)));
                // Tenant-realm token: fleet flags must be dropped even
                // though the claim asserts fleet_admin.
                assert!(!fleet_admin, "tenant realm must not confer fleet_admin");
                assert!(!is_root);
                // Absent broad scope -> least privilege, never None.
                assert_eq!(scope, Some(ApiKeyScope::ReadOnly));
                assert!(bound_cn.is_none());
            }
            Principal::Anonymous => panic!("expected operator"),
        }
    }

    #[test]
    fn tenant_realm_cannot_carry_fleet_root() {
        let mut c = claims();
        c.realm_scope = RealmScope::Tenant;
        c.is_root = true;
        c.fleet_admin = true;
        match principal_from_claims(c) {
            Principal::Operator {
                is_root,
                fleet_admin,
                ..
            } => {
                assert!(!is_root, "tenant realm must not confer is_root");
                assert!(!fleet_admin, "tenant realm must not confer fleet_admin");
            }
            Principal::Anonymous => panic!("expected operator"),
        }
    }

    #[test]
    fn system_realm_keeps_fleet_root() {
        let mut c = claims();
        c.realm_scope = RealmScope::System;
        c.is_root = true;
        c.fleet_admin = true;
        match principal_from_claims(c) {
            Principal::Operator {
                is_root,
                fleet_admin,
                ..
            } => {
                assert!(is_root, "system realm preserves is_root");
                assert!(fleet_admin, "system realm preserves fleet_admin");
            }
            Principal::Anonymous => panic!("expected operator"),
        }
    }

    #[test]
    fn explicit_admin_scope_maps_to_full() {
        let mut c = claims();
        c.scope = Some("openid admin".to_string());
        match principal_from_claims(c) {
            Principal::Operator { scope, .. } => {
                assert_eq!(scope, Some(ApiKeyScope::Full));
            }
            Principal::Anonymous => panic!("expected operator"),
        }
    }

    #[test]
    fn cnf_cn_binds_principal() {
        use identity_token::claims::Confirmation;
        let mut c = claims();
        let cn = Uuid::from_u128(0x99);
        c.cnf = Some(Confirmation { cn: Some(cn) });
        match principal_from_claims(c) {
            Principal::Operator { bound_cn, .. } => {
                assert_eq!(bound_cn, Some(cn));
            }
            Principal::Anonymous => panic!("expected operator"),
        }
    }

    #[test]
    fn verifier_derives_jwks_url_from_issuer() {
        let v = IdentitydVerifier::new("http://127.0.0.1:8090/realms/r");
        assert_eq!(v.issuer(), "http://127.0.0.1:8090/realms/r");
    }
}
