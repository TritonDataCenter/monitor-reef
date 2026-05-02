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
    Authorizer, Context as CedarContext, Decision as CedarDecision, Entities, Entity, EntityUid,
    PolicySet, Request, RestrictedExpression,
};
use chrono::Utc;
use dropshot::{ClientErrorStatusCode, HttpError, RequestContext};
use tracing::warn;
use tritond_audit::Decision as AuditDecision;
use tritond_auth::{
    JwtKey, OidcConfig, OidcVerifier, TokenKind, parse_api_key, peek_issuer, verify,
    verify_api_key_secret,
};
use tritond_store::{Federation, Store, StoreError, User};
use uuid::Uuid;

use crate::audit::AuditService;

/// Embedded Cedar policy bundle.
///
/// Three rules, ordered by specificity:
///
/// * Anonymous callers can hit `health`, `login`, and `refresh`.
/// * Authenticated operators with `is_root == true` can perform any
///   action (the bootstrap-root path).
/// * Federated users in a silo can perform a hand-curated set of
///   tenant-scoped actions when `principal.silo_id ==
///   resource.silo_id`. Adding a new tenant-facing action means
///   appending it to the action list in this rule.
///
/// Every other access falls through to Cedar's default deny.
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

@id("silo-member-allows-silo-scoped-tenant-actions")
permit(
    principal,
    action in [
        Action::"project_list",
        Action::"project_create",
        Action::"project_get",
        Action::"project_delete",
        Action::"vpc_list",
        Action::"vpc_create",
        Action::"vpc_get",
        Action::"vpc_delete",
        Action::"subnet_list",
        Action::"subnet_create",
        Action::"subnet_get",
        Action::"subnet_delete"
    ],
    resource
) when {
    principal has silo_id &&
    resource has silo_id &&
    principal.silo_id == resource.silo_id
};
"#;

/// Result of authenticating an inbound request.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Principal {
    /// Authenticated operator or federated user. `is_root` is true
    /// for the bootstrap operator and any other cluster-wide
    /// account; `silo_id` is `Some(...)` for federated users (and,
    /// in future, for silo-scoped admin operators).
    Operator {
        user_id: Uuid,
        is_root: bool,
        silo_id: Option<Uuid>,
    },
    /// No valid credential was presented (or the presented one was
    /// invalid). Cedar will allow this principal only on
    /// public-action endpoints.
    Anonymous,
}

/// Errors that can come out of [`AuthService::authenticate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The backing store reported a failure that the auth path
    /// can't paper over (e.g. FoundationDB unreachable). We do **not**
    /// downgrade these to anonymous, because then a partial outage
    /// would silently de-authenticate every caller and produce 403
    /// noise instead of an honest 503.
    #[error("auth backend unavailable: {0}")]
    Backend(StoreError),
}

impl From<AuthError> for HttpError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::Backend(inner) => {
                HttpError::for_internal_error(format!("auth backend unavailable: {inner}"))
            }
        }
    }
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

    /// Cedar entity carrying the principal's attributes (`is_root`
    /// for bootstrap-style accounts, `silo_id` for silo-scoped ones).
    fn entity(&self) -> Result<Entity> {
        let uid = self.entity_uid()?;
        let mut attrs: HashMap<String, RestrictedExpression> = HashMap::new();
        if let Principal::Operator {
            is_root, silo_id, ..
        } = self
        {
            attrs.insert(
                "is_root".to_string(),
                RestrictedExpression::new_bool(*is_root),
            );
            if let Some(silo_id) = silo_id {
                attrs.insert(
                    "silo_id".to_string(),
                    RestrictedExpression::new_string(silo_id.to_string()),
                );
            }
        }
        Entity::new(uid, attrs, HashSet::new()).context("constructing principal entity")
    }
}

/// Stable identifier for a Cedar action — one entry per endpoint.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Action {
    Health,
    Login,
    Refresh,
    CreateSilo,
    GetSilo,
    CreateApiKey,
    ListApiKeys,
    DeleteApiKey,
    AuditList,
    AuditFetch,
    AuditVerify,
    SiloIdpSet,
    SiloIdpGet,
    SiloIdpDelete,
    ProjectList,
    ProjectCreate,
    ProjectGet,
    ProjectDelete,
    VpcList,
    VpcCreate,
    VpcGet,
    VpcDelete,
    SubnetList,
    SubnetCreate,
    SubnetGet,
    SubnetDelete,
}

impl Action {
    /// Stable string identifier used in Cedar policies and audit
    /// events. Public so the audit emitter can name the action it
    /// just gated on without redoing the match.
    #[must_use]
    pub fn cedar_id(self) -> &'static str {
        match self {
            Action::Health => "health",
            Action::Login => "login",
            Action::Refresh => "refresh",
            Action::CreateSilo => "create_silo",
            Action::GetSilo => "get_silo",
            Action::CreateApiKey => "create_api_key",
            Action::ListApiKeys => "list_api_keys",
            Action::DeleteApiKey => "delete_api_key",
            Action::AuditList => "audit_list",
            Action::AuditFetch => "audit_fetch",
            Action::AuditVerify => "audit_verify",
            Action::SiloIdpSet => "silo_idp_set",
            Action::SiloIdpGet => "silo_idp_get",
            Action::SiloIdpDelete => "silo_idp_delete",
            Action::ProjectList => "project_list",
            Action::ProjectCreate => "project_create",
            Action::ProjectGet => "project_get",
            Action::ProjectDelete => "project_delete",
            Action::VpcList => "vpc_list",
            Action::VpcCreate => "vpc_create",
            Action::VpcGet => "vpc_get",
            Action::VpcDelete => "vpc_delete",
            Action::SubnetList => "subnet_list",
            Action::SubnetCreate => "subnet_create",
            Action::SubnetGet => "subnet_get",
            Action::SubnetDelete => "subnet_delete",
        }
    }

    fn entity_uid(self) -> Result<EntityUid> {
        EntityUid::from_str(&format!("Action::\"{}\"", self.cedar_id()))
            .context("constructing action entity uid")
    }
}

/// Per-cluster auth service: holds the JWT signing key, the parsed
/// Cedar policy set, the Cedar `Authorizer`, and the OIDC verifier
/// shared across silos (cheap to reuse across requests).
pub struct AuthService {
    jwt_key: JwtKey,
    policy_set: PolicySet,
    authorizer: Authorizer,
    oidc: OidcVerifier,
}

impl AuthService {
    pub fn new(jwt_key: JwtKey) -> Result<Self> {
        let policy_set = PolicySet::from_str(POLICY_BUNDLE)
            .map_err(|e| anyhow::anyhow!("parse Cedar policy bundle: {e}"))?;
        Ok(Self {
            jwt_key,
            policy_set,
            authorizer: Authorizer::new(),
            oidc: OidcVerifier::new(),
        })
    }

    pub fn jwt_key(&self) -> &JwtKey {
        &self.jwt_key
    }

    pub fn oidc(&self) -> &OidcVerifier {
        &self.oidc
    }

    /// Authenticate the inbound request.
    ///
    /// Returns:
    /// * [`Principal::Operator`] on a valid credential.
    /// * [`Principal::Anonymous`] on missing, malformed, expired, or
    ///   unknown credentials — anything that points at the user
    ///   rather than the system.
    /// * [`AuthError::Backend`] when the store itself fails. The
    ///   caller maps this to a 5xx so a half-broken cluster does not
    ///   silently deauthenticate every caller as 403.
    pub async fn authenticate(
        &self,
        store: &dyn Store,
        bearer: Option<&str>,
    ) -> Result<Principal, AuthError> {
        let Some(token) = bearer else {
            return Ok(Principal::Anonymous);
        };

        if token.starts_with(tritond_auth::API_KEY_PREFIX) {
            self.authenticate_api_key(store, token).await
        } else {
            self.authenticate_jwt(store, token).await
        }
    }

    async fn authenticate_jwt(
        &self,
        store: &dyn Store,
        token: &str,
    ) -> Result<Principal, AuthError> {
        // Operator tokens (HS256, our signing key) come first; if the
        // token isn't one of ours, fall through to OIDC against
        // configured silo IdPs.
        match verify(&self.jwt_key, token, TokenKind::Access) {
            Ok(claims) => match store.get_user_by_id(claims.sub).await {
                Ok(user) => Ok(Principal::Operator {
                    user_id: user.id,
                    is_root: user.is_root,
                    silo_id: user.silo_id,
                }),
                Err(StoreError::NotFound) => Ok(Principal::Anonymous),
                Err(e) => {
                    warn!(error = %e, "store failure while resolving JWT principal");
                    Err(AuthError::Backend(e))
                }
            },
            Err(_) => self.authenticate_oidc(store, token).await,
        }
    }

    async fn authenticate_oidc(
        &self,
        store: &dyn Store,
        token: &str,
    ) -> Result<Principal, AuthError> {
        // Cheaply peek at the `iss` claim so we know which silo's
        // IdP to verify against. A token without a parseable `iss`
        // is just anonymous; same goes for one whose issuer doesn't
        // match any configured silo.
        let Some(issuer) = peek_issuer(token) else {
            return Ok(Principal::Anonymous);
        };
        let configs = match store.list_idp_configs().await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "store failure listing idp configs");
                return Err(AuthError::Backend(e));
            }
        };
        let Some((silo_id, idp)) = configs
            .into_iter()
            .find(|(_, cfg)| cfg.issuer_url == issuer)
        else {
            return Ok(Principal::Anonymous);
        };

        let oidc_cfg = OidcConfig {
            issuer_url: idp.issuer_url,
            client_id: idp.client_id,
            client_secret: idp.client_secret,
            audience: idp.audience,
        };
        let cache_key = silo_id.to_string();
        let claims = match self.oidc.verify(&cache_key, &oidc_cfg, token).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, %silo_id, "rejecting oidc token as anonymous");
                return Ok(Principal::Anonymous);
            }
        };

        // JIT user lookup or create for this (silo, issuer, subject).
        let user = match store
            .get_user_by_federation(silo_id, &claims.issuer, &claims.subject)
            .await
        {
            Ok(user) => user,
            Err(StoreError::NotFound) => {
                let new_user = User {
                    id: Uuid::new_v4(),
                    // Disambiguate username across silos so a tenant
                    // user with the same email in two silos doesn't
                    // collide on the global username uniqueness key.
                    username: format!("{}@{silo_id}", claims.username),
                    password_hash: String::new(),
                    is_root: false,
                    created_at: Utc::now(),
                    silo_id: Some(silo_id),
                    federation: Some(Federation {
                        issuer: claims.issuer.clone(),
                        subject: claims.subject.clone(),
                    }),
                };
                match store.create_user(new_user).await {
                    Ok(u) => u,
                    Err(StoreError::Conflict(_)) => {
                        // A concurrent first login won the race. Re-read.
                        store
                            .get_user_by_federation(silo_id, &claims.issuer, &claims.subject)
                            .await
                            .map_err(|e| {
                                warn!(error = %e, "post-conflict refetch failed");
                                AuthError::Backend(e)
                            })?
                    }
                    Err(e) => {
                        warn!(error = %e, "JIT create_user failed");
                        return Err(AuthError::Backend(e));
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "store failure resolving federated user");
                return Err(AuthError::Backend(e));
            }
        };

        Ok(Principal::Operator {
            user_id: user.id,
            is_root: user.is_root,
            silo_id: user.silo_id,
        })
    }

    async fn authenticate_api_key(
        &self,
        store: &dyn Store,
        token: &str,
    ) -> Result<Principal, AuthError> {
        let Some((lookup_id, secret)) = parse_api_key(token) else {
            return Ok(Principal::Anonymous);
        };
        let record = match store.get_api_key_by_lookup_id(lookup_id).await {
            Ok(record) => record,
            Err(StoreError::NotFound) => return Ok(Principal::Anonymous),
            Err(e) => {
                warn!(error = %e, "store failure while resolving api key by lookup id");
                return Err(AuthError::Backend(e));
            }
        };
        let verified = match verify_api_key_secret(secret, &record.hash).await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "bcrypt failure while verifying api key");
                return Ok(Principal::Anonymous);
            }
        };
        if !verified {
            return Ok(Principal::Anonymous);
        }
        match store.get_user_by_id(record.user_id).await {
            Ok(user) => Ok(Principal::Operator {
                user_id: user.id,
                is_root: user.is_root,
                silo_id: user.silo_id,
            }),
            Err(StoreError::NotFound) => Ok(Principal::Anonymous),
            Err(e) => {
                warn!(error = %e, "store failure while resolving api-key user");
                Err(AuthError::Backend(e))
            }
        }
    }

    /// Evaluate the embedded Cedar policy against `System::"global"`.
    /// Returns `Ok(())` on permit, `Err(403)` on deny. Used for
    /// fleet-scoped actions (no silo in the URL path).
    pub fn authorize(&self, principal: &Principal, action: Action) -> Result<(), HttpError> {
        let resource_uid = EntityUid::from_str("System::\"global\"")
            .map_err(|e| HttpError::for_internal_error(format!("resource uid: {e}")))?;
        let resource_entity = Entity::new(resource_uid.clone(), HashMap::new(), HashSet::new())
            .map_err(|e| HttpError::for_internal_error(format!("resource entity: {e}")))?;
        match self.evaluate(principal, action, resource_uid, resource_entity)? {
            CedarDecision::Allow => Ok(()),
            CedarDecision::Deny => Err(forbidden_for(action)),
        }
    }

    /// Evaluate the policy against a `Silo::"<silo_id>"` resource
    /// carrying a `silo_id` attribute, so the silo-membership rule
    /// can fire. Returns `Ok(())` on permit; on deny, returns **404
    /// Not Found** rather than 403 — a federated user from silo A
    /// hitting silo B's resources should not be able to learn that
    /// silo B exists.
    pub fn authorize_in_silo(
        &self,
        principal: &Principal,
        action: Action,
        silo_id: Uuid,
    ) -> Result<(), HttpError> {
        let resource_uid = EntityUid::from_str(&format!("Silo::\"{silo_id}\""))
            .map_err(|e| HttpError::for_internal_error(format!("silo resource uid: {e}")))?;
        let mut attrs = HashMap::new();
        attrs.insert(
            "silo_id".to_string(),
            RestrictedExpression::new_string(silo_id.to_string()),
        );
        let resource_entity = Entity::new(resource_uid.clone(), attrs, HashSet::new())
            .map_err(|e| HttpError::for_internal_error(format!("silo resource entity: {e}")))?;
        match self.evaluate(principal, action, resource_uid, resource_entity)? {
            CedarDecision::Allow => Ok(()),
            CedarDecision::Deny => Err(not_found_in_silo()),
        }
    }

    fn evaluate(
        &self,
        principal: &Principal,
        action: Action,
        resource_uid: EntityUid,
        resource_entity: Entity,
    ) -> Result<CedarDecision, HttpError> {
        let principal_uid = principal
            .entity_uid()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let action_uid = action
            .entity_uid()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let principal_entity = principal
            .entity()
            .map_err(|e| HttpError::for_internal_error(e.to_string()))?;
        let entities = Entities::from_entities([principal_entity, resource_entity], None)
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
        Ok(response.decision())
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
///
/// Emits exactly one audit event per call:
/// - Cedar **Allow** on any principal → logs Allow.
/// - Cedar **Deny** on an authenticated principal → logs Deny.
/// - Cedar **Deny** on an anonymous principal → does not log
///   (probe noise; see [`crate::audit::AuditService::record_decision`]).
pub async fn authenticate_and_authorize<C>(
    rqctx: &RequestContext<C>,
    auth: &AuthService,
    audit: &AuditService,
    store: &Arc<dyn Store>,
    action: Action,
) -> Result<Principal, HttpError>
where
    C: dropshot::ServerContext,
{
    let bearer = extract_bearer(rqctx);
    let principal = auth.authenticate(store.as_ref(), bearer.as_deref()).await?;
    let request_id = Uuid::parse_str(&rqctx.request_id).ok();
    match auth.authorize(&principal, action) {
        Ok(()) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Allow)
                .await;
            Ok(principal)
        }
        Err(http_err) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Deny)
                .await;
            Err(http_err)
        }
    }
}

/// 401 helper — used by handlers that need an *authenticated*
/// principal even if Cedar would let an anonymous request through
/// (e.g. /v2/auth/api-keys must run as somebody).
pub fn require_authenticated(principal: Principal) -> Result<(Uuid, bool), HttpError> {
    match principal {
        Principal::Operator {
            user_id, is_root, ..
        } => Ok((user_id, is_root)),
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

/// Cross-silo deny: return 404 so cross-tenant probes can't enumerate
/// silos. The shape matches resource-not-found errors from
/// `store_error_to_http`, which is intentional.
fn not_found_in_silo() -> HttpError {
    HttpError::for_client_error(
        Some("NotFound".to_string()),
        ClientErrorStatusCode::NOT_FOUND,
        "not found".to_string(),
    )
}

/// Silo-scoped variant of [`authenticate_and_authorize`]. The Cedar
/// resource is `Silo::"<silo_id>"` so the `silo-member-allows-silo-
/// scoped-tenant-actions` rule can fire; deny returns **404** rather
/// than 403 so cross-tenant probes can't enumerate silos.
pub async fn authenticate_and_authorize_in_silo<C>(
    rqctx: &RequestContext<C>,
    auth: &AuthService,
    audit: &AuditService,
    store: &Arc<dyn Store>,
    action: Action,
    silo_id: Uuid,
) -> Result<Principal, HttpError>
where
    C: dropshot::ServerContext,
{
    let bearer = extract_bearer(rqctx);
    let principal = auth.authenticate(store.as_ref(), bearer.as_deref()).await?;
    let request_id = Uuid::parse_str(&rqctx.request_id).ok();
    match auth.authorize_in_silo(&principal, action, silo_id) {
        Ok(()) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Allow)
                .await;
            Ok(principal)
        }
        Err(http_err) => {
            audit
                .record_decision(&principal, action, request_id, AuditDecision::Deny)
                .await;
            Err(http_err)
        }
    }
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
            silo_id: None,
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
            silo_id: None,
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
            silo_id: None,
            federation: None,
        };
        let user_id = user.id;
        store.create_user(user).await.unwrap();
        let (token, _) = mint_access(auth.jwt_key(), user_id).unwrap();

        let p = auth.authenticate(&store, Some(&token)).await.unwrap();
        match p {
            Principal::Operator {
                user_id: got_id,
                is_root,
                ..
            } => {
                assert_eq!(got_id, user_id);
                assert!(is_root);
            }
            Principal::Anonymous => panic!("expected operator"),
        }
    }

    #[tokio::test]
    async fn jwt_for_unknown_user_yields_anonymous() {
        let auth = fresh_service();
        let store = MemStore::new();
        let (token, _) = mint_access(auth.jwt_key(), Uuid::new_v4()).unwrap();
        let p = auth.authenticate(&store, Some(&token)).await.unwrap();
        assert!(matches!(p, Principal::Anonymous));
    }

    #[tokio::test]
    async fn bogus_jwt_yields_anonymous() {
        let auth = fresh_service();
        let store = MemStore::new();
        let p = auth.authenticate(&store, Some("not.a.jwt")).await.unwrap();
        assert!(matches!(p, Principal::Anonymous));
    }

    #[tokio::test]
    async fn malformed_api_key_token_yields_anonymous() {
        let auth = fresh_service();
        let store = MemStore::new();
        // Right prefix, wrong length: not a real api key.
        let p = auth
            .authenticate(&store, Some("tcadm_short"))
            .await
            .unwrap();
        assert!(matches!(p, Principal::Anonymous));
    }
}
