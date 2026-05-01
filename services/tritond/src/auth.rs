// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Authentication and authorization for tritond.
//!
//! This module pulls together three things at request time:
//!
//! 1. **Authentication.** The `Authorization: Bearer …` header is
//!    inspected. Tokens beginning with [`tritond_auth::API_KEY_PREFIX`]
//!    are looked up against bcrypt-hashed records in the store; other
//!    tokens are validated as HS256 JWTs against the cluster's
//!    operator signing key.
//! 2. **Principal construction.** Authenticated requests yield an
//!    `Operator` entity carrying an `is_root` attribute drawn from the
//!    user record; unauthenticated requests yield an `Anonymous`
//!    entity.
//! 3. **Authorization.** A Cedar `PolicySet` evaluates the request.
//!    Phase 0e ships a deliberately small embedded bundle: anonymous
//!    callers can hit health, login, and refresh; root operators can
//!    do anything; everything else is denied.
//!
//! When per-silo OIDC and finer-grained policies arrive, the entity
//! model expands but the call shape (`AuthService::authenticate` →
//! `AuthService::authorize`) stays the same.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use cedar_policy::{
    Authorizer, Context as CedarContext, Decision, Entities, Entity, EntityUid, PolicySet, Request,
    RestrictedExpression,
};
use dropshot::{ClientErrorStatusCode, HttpError, RequestContext};
use tritond_auth::{JwtKey, TokenKind, verify, verify_api_key};
use tritond_store::Store;
use uuid::Uuid;

/// Embedded Cedar policy bundle.
///
/// The set is intentionally small for Phase 0e:
///
/// * Anonymous callers can hit `health`, `login`, and `refresh`.
/// * Authenticated operators with `is_root == true` can perform any
///   action.
/// * Every other access is denied by Cedar's default deny.
const POLICY_BUNDLE: &str = r#"
@id("anonymous-public-actions")
permit(
    principal,
    action in [Action::"health", Action::"login", Action::"refresh"],
    resource
);

@id("root-operator-allows-all")
permit(
    principal,
    action,
    resource
) when {
    principal has is_root && principal.is_root == true
};
"#;

/// Result of authenticating an inbound request.
#[derive(Debug, Clone)]
pub enum Principal {
    /// Authenticated operator. `is_root` is captured at lookup time.
    Operator { user_id: Uuid, is_root: bool },
    /// No valid credential was presented.
    Anonymous,
}

impl Principal {
    /// Cedar entity uid for this principal, e.g. `Operator::"<uuid>"`.
    fn entity_uid(&self) -> Result<EntityUid> {
        let raw = match self {
            Principal::Operator { user_id, .. } => format!("Operator::\"{user_id}\""),
            Principal::Anonymous => "Anonymous::\"anon\"".to_string(),
        };
        EntityUid::from_str(&raw).context("constructing principal entity uid")
    }

    /// Cedar entity carrying the principal's attributes (e.g. `is_root`).
    fn entity(&self) -> Result<Entity> {
        let uid = self.entity_uid()?;
        let mut attrs: HashMap<String, RestrictedExpression> = HashMap::new();
        if let Principal::Operator { is_root, .. } = self {
            attrs.insert(
                "is_root".to_string(),
                RestrictedExpression::new_bool(*is_root),
            );
        }
        Entity::new(uid, attrs, HashSet::new()).context("constructing principal entity")
    }
}

/// Stable identifier for a Cedar action — one entry per endpoint.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Health,
    Login,
    Refresh,
    CreateSilo,
    GetSilo,
    CreateApiKey,
    ListApiKeys,
    DeleteApiKey,
}

impl Action {
    fn cedar_id(self) -> &'static str {
        match self {
            Action::Health => "health",
            Action::Login => "login",
            Action::Refresh => "refresh",
            Action::CreateSilo => "create_silo",
            Action::GetSilo => "get_silo",
            Action::CreateApiKey => "create_api_key",
            Action::ListApiKeys => "list_api_keys",
            Action::DeleteApiKey => "delete_api_key",
        }
    }

    fn entity_uid(self) -> Result<EntityUid> {
        EntityUid::from_str(&format!("Action::\"{}\"", self.cedar_id()))
            .context("constructing action entity uid")
    }
}

/// Per-cluster auth service: holds the JWT signing key, the parsed
/// Cedar policy set, and a Cedar `Authorizer` (the latter is cheap to
/// reuse across requests).
pub struct AuthService {
    jwt_key: JwtKey,
    policy_set: PolicySet,
    authorizer: Authorizer,
}

impl AuthService {
    pub fn new(jwt_key: JwtKey) -> Result<Self> {
        let policy_set = PolicySet::from_str(POLICY_BUNDLE)
            .map_err(|e| anyhow::anyhow!("parse Cedar policy bundle: {e}"))?;
        Ok(Self {
            jwt_key,
            policy_set,
            authorizer: Authorizer::new(),
        })
    }

    pub fn jwt_key(&self) -> &JwtKey {
        &self.jwt_key
    }

    /// Authenticate the inbound request. Never returns an error: a
    /// missing or invalid credential simply yields [`Principal::Anonymous`]
    /// so that downstream Cedar rules can decide what to do with it.
    pub async fn authenticate(&self, store: &dyn Store, bearer: Option<&str>) -> Principal {
        let Some(token) = bearer else {
            return Principal::Anonymous;
        };

        if token.starts_with(tritond_auth::API_KEY_PREFIX) {
            self.authenticate_api_key(store, token).await
        } else {
            self.authenticate_jwt(store, token).await
        }
    }

    async fn authenticate_jwt(&self, store: &dyn Store, token: &str) -> Principal {
        let Ok(claims) = verify(&self.jwt_key, token, TokenKind::Access) else {
            return Principal::Anonymous;
        };
        match store.get_user_by_id(claims.sub).await {
            Ok(user) => Principal::Operator {
                user_id: user.id,
                is_root: user.is_root,
            },
            Err(_) => Principal::Anonymous,
        }
    }

    async fn authenticate_api_key(&self, store: &dyn Store, token: &str) -> Principal {
        // Linear scan — fine while the cluster has handfuls of keys;
        // Phase 1 indexes by a key prefix to avoid the bcrypt-per-key
        // cost on every request.
        let keys = match store.all_api_keys().await {
            Ok(k) => k,
            Err(_) => return Principal::Anonymous,
        };
        for record in keys {
            if matches!(verify_api_key(token, &record.hash), Ok(true)) {
                return match store.get_user_by_id(record.user_id).await {
                    Ok(user) => Principal::Operator {
                        user_id: user.id,
                        is_root: user.is_root,
                    },
                    Err(_) => Principal::Anonymous,
                };
            }
        }
        Principal::Anonymous
    }

    /// Evaluate the embedded Cedar policy. Returns `Ok(())` on
    /// permit, `Err(403)` on deny.
    pub fn authorize(&self, principal: &Principal, action: Action) -> Result<(), HttpError> {
        let principal_uid = principal
            .entity_uid()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let action_uid = action
            .entity_uid()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let resource_uid = EntityUid::from_str("System::\"global\"")
            .map_err(|e| HttpError::for_internal_error(format!("resource uid: {e}")))?;

        let entity = principal
            .entity()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let entities = Entities::from_entities([entity], None)
            .map_err(|e| HttpError::for_internal_error(format!("entities: {e}")))?;

        let request = Request::new(
            principal_uid,
            action_uid,
            resource_uid,
            CedarContext::empty(),
            None,
        )
        .map_err(|e| HttpError::for_internal_error(format!("cedar request: {e}")))?;

        let response = self
            .authorizer
            .is_authorized(&request, &self.policy_set, &entities);
        match response.decision() {
            Decision::Allow => Ok(()),
            Decision::Deny => Err(forbidden_for(action)),
        }
    }
}

/// Helper: pull a `Bearer <token>` value out of the request's
/// `Authorization` header, if present.
fn extract_bearer<C>(rqctx: &RequestContext<C>) -> Option<String>
where
    C: dropshot::ServerContext,
{
    let raw = rqctx
        .request
        .headers()
        .get(http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = raw.strip_prefix("Bearer ")?;
    Some(token.trim().to_string())
}

/// Authenticate then authorize a request in one shot. Returns the
/// principal so handlers that care about identity (e.g. `create_api_key`,
/// `list_api_keys`) can use it without a second round trip.
pub async fn authenticate_and_authorize<C>(
    rqctx: &RequestContext<C>,
    auth: &AuthService,
    store: &Arc<dyn Store>,
    action: Action,
) -> Result<Principal, HttpError>
where
    C: dropshot::ServerContext,
{
    let bearer = extract_bearer(rqctx);
    let principal = auth.authenticate(store.as_ref(), bearer.as_deref()).await;
    auth.authorize(&principal, action)?;
    Ok(principal)
}

/// 401 helper — used by handlers that need an *authenticated*
/// principal even if Cedar would let an anonymous request through
/// (e.g. /v2/auth/api-keys must run as somebody).
pub fn require_authenticated(principal: Principal) -> Result<(Uuid, bool), HttpError> {
    match principal {
        Principal::Operator { user_id, is_root } => Ok((user_id, is_root)),
        Principal::Anonymous => Err(HttpError::for_client_error(
            Some("Unauthenticated".to_string()),
            ClientErrorStatusCode::UNAUTHORIZED,
            "authentication required".to_string(),
        )),
    }
}

fn forbidden_for(action: Action) -> HttpError {
    HttpError::for_client_error(
        Some("Forbidden".to_string()),
        ClientErrorStatusCode::FORBIDDEN,
        format!("not authorised for {}", action.cedar_id()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tritond_auth::{JwtKey, mint_access};
    use tritond_store::{MemStore, User};

    fn fresh_service() -> AuthService {
        AuthService::new(JwtKey::generate()).unwrap()
    }

    #[tokio::test]
    async fn anonymous_can_hit_public_actions() {
        let auth = fresh_service();
        for action in [Action::Health, Action::Login, Action::Refresh] {
            assert!(auth.authorize(&Principal::Anonymous, action).is_ok());
        }
    }

    #[tokio::test]
    async fn anonymous_cannot_create_silo() {
        let auth = fresh_service();
        let err = auth
            .authorize(&Principal::Anonymous, Action::CreateSilo)
            .expect_err("anonymous should be denied");
        assert_eq!(err.status_code.as_status().as_u16(), 403);
    }

    #[tokio::test]
    async fn root_operator_can_do_anything() {
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: true,
        };
        for action in [
            Action::CreateSilo,
            Action::GetSilo,
            Action::CreateApiKey,
            Action::ListApiKeys,
            Action::DeleteApiKey,
        ] {
            assert!(auth.authorize(&p, action).is_ok(), "denied {action:?}");
        }
    }

    #[tokio::test]
    async fn non_root_operator_is_denied_outside_public_actions() {
        let auth = fresh_service();
        let p = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: false,
        };
        assert!(auth.authorize(&p, Action::Health).is_ok());
        let err = auth
            .authorize(&p, Action::CreateSilo)
            .expect_err("non-root should be denied");
        assert_eq!(err.status_code.as_status().as_u16(), 403);
    }

    #[tokio::test]
    async fn jwt_authenticates_to_operator() {
        let auth = fresh_service();
        let store = MemStore::new();
        let user = User {
            id: Uuid::new_v4(),
            username: "root".to_string(),
            password_hash: "$2y$12$dummy".to_string(),
            is_root: true,
            created_at: chrono::Utc::now(),
        };
        let user_id = user.id;
        store.create_user(user).await.unwrap();
        let (token, _) = mint_access(auth.jwt_key(), user_id).unwrap();

        let p = auth.authenticate(&store, Some(&token)).await;
        match p {
            Principal::Operator {
                user_id: got_id,
                is_root,
            } => {
                assert_eq!(got_id, user_id);
                assert!(is_root);
            }
            Principal::Anonymous => panic!("expected operator"),
        }
    }

    #[tokio::test]
    async fn bogus_jwt_yields_anonymous() {
        let auth = fresh_service();
        let store = MemStore::new();
        let p = auth.authenticate(&store, Some("not.a.jwt")).await;
        assert!(matches!(p, Principal::Anonymous));
    }
}
