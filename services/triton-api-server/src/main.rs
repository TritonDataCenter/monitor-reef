// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::{Context, Result};
use cloudapi_client::{AuthConfig, KeySource, TypedClient};
use dropshot::{
    ClientErrorStatusCode, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError,
    HttpResponseAccepted, HttpResponseCreated, HttpResponseDeleted, HttpResponseHeaders,
    HttpResponseOk, HttpServerStarter, Path, RequestContext, TypedBody, WebsocketChannelResult,
    WebsocketConnection,
};
use secrecy::SecretString;
use serde::Deserialize;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Role as WsRole;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;
use tracing::{info, warn};
use triton_api::{
    AddNodesRequest, BootstrapClusterRequest, ChallengeMethod, Cluster, ClusterList, ClusterPath,
    ClusterState, CreateClusterRequest, Jwk, JwkSet, KubeconfigResponse, LoginChallenge,
    LoginOutcome, LoginRequest, LoginResponse, LoginVerifyRequest, LogoutResponse,
    NodeBootstrapRole, NodeBootstrapSpec, PingResponse, RefreshRequest, RefreshResponse,
    SessionResponse, TritonApi, UpgradeClusterRequest, UserInfo,
};
use triton_auth::{auth_scheme, http_sig};
use triton_auth_session::{
    JwtConfig as SessionJwtConfig, JwtService, LdapConfig as SessionLdapConfig, LdapService,
    MahiService, Role, SessionError, verify_totp,
};
use triton_relay_protocol::{WsCompat, bridge, read_connect_target, write_connect_target};
use uuid::Uuid;

mod cluster_store;
use cluster_store::{
    ClusterRecord, ClusterStore, FileClusterStore, NodeInfo, NodeRole, StoreError,
};
mod talos;
mod talos_config;

mod relay;
use relay::RelayState;

/// Default request body size limit: 10 MiB.
const DEFAULT_MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Deserialize)]
#[allow(dead_code)]
struct ApiServerConfig {
    #[serde(default)]
    datacenter_name: Option<String>,
    #[serde(default)]
    instance_uuid: Option<String>,
    #[serde(default)]
    server_uuid: Option<String>,
    #[serde(default)]
    admin_ip: Option<String>,
    #[serde(default = "default_bind_address")]
    bind_address: String,
    #[serde(default)]
    max_body_bytes: Option<u64>,
    #[serde(default)]
    ldap: Option<LdapConfigFile>,
    #[serde(default)]
    mahi: Option<MahiConfigFile>,
    #[serde(default)]
    jwt: Option<JwtConfigFile>,
    #[serde(default)]
    clusters: ClustersConfigFile,
    #[serde(default)]
    cloudapi: Option<CloudApiConfigFile>,
    /// When set, unauthenticated requests are treated as this account UUID.
    /// For local development only — never set this in production.
    #[serde(default)]
    dev_account_uuid: Option<Uuid>,
}

/// Operator-credential config for the server-side CloudAPI client.
///
/// When present, tritonapi will provision VMs on behalf of callers via
/// CloudAPI using these operator credentials. When absent, the bootstrap
/// endpoint returns 503.
#[derive(Deserialize)]
struct CloudApiConfigFile {
    /// CloudAPI base URL (e.g. `https://cloudapi.example.com`).
    url: url::Url,
    /// Operator account login (e.g. `admin`).
    account: String,
    /// Path to the PEM-encoded private key file on disk.
    key_file: std::path::PathBuf,
    /// Optional key fingerprint. If omitted, the key_file is used directly
    /// without fingerprint matching.
    #[serde(default)]
    key_fingerprint: Option<String>,
}

#[derive(Deserialize)]
struct ClustersConfigFile {
    /// Directory holding one JSON file per Kelp cluster record.
    /// Defaults to `./data/clusters` for local dev; production
    /// deployments should point this at a persistent volume.
    #[serde(default = "default_clusters_state_dir")]
    state_dir: PathBuf,
}

impl Default for ClustersConfigFile {
    fn default() -> Self {
        Self {
            state_dir: default_clusters_state_dir(),
        }
    }
}

fn default_clusters_state_dir() -> PathBuf {
    PathBuf::from("./data/clusters")
}

#[derive(Deserialize)]
struct LdapConfigFile {
    url: url::Url,
    bind_dn: String,
    bind_password: SecretString,
    search_base: String,
    #[serde(default = "default_tls_verify")]
    tls_verify: bool,
    #[serde(default = "default_ldap_timeout_secs")]
    connection_timeout_secs: NonZeroU64,
}

#[derive(Deserialize)]
struct MahiConfigFile {
    url: url::Url,
}

#[derive(Deserialize)]
struct JwtConfigFile {
    private_key_file: String,
    public_key_file: String,
    #[serde(default = "default_access_ttl_secs")]
    access_ttl_secs: u64,
    #[serde(default = "default_refresh_ttl_secs")]
    refresh_ttl_secs: u64,
}

fn default_bind_address() -> String {
    "127.0.0.1:8080".to_string()
}
fn default_tls_verify() -> bool {
    true
}
fn default_ldap_timeout_secs() -> NonZeroU64 {
    // Fallback is only reached if `10` is somehow zero; falling back to
    // `NonZeroU64::MIN` (= 1) is strictly safer than panicking.
    NonZeroU64::new(10).unwrap_or(NonZeroU64::MIN)
}
fn default_access_ttl_secs() -> u64 {
    3600
}
fn default_refresh_ttl_secs() -> u64 {
    86400
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            datacenter_name: None,
            instance_uuid: None,
            server_uuid: None,
            admin_ip: None,
            bind_address: default_bind_address(),
            max_body_bytes: None,
            ldap: None,
            mahi: None,
            jwt: None,
            clusters: ClustersConfigFile::default(),
            cloudapi: None,
            dev_account_uuid: None,
        }
    }
}

/// Load config from TRITON__CONFIG_FILE env var.
///
/// If the env var is unset, returns defaults (useful for dev). If set but
/// unreadable or unparseable, returns an error so SMF marks the service in
/// maintenance.
async fn load_config() -> Result<ApiServerConfig> {
    let Some(path) = std::env::var("TRITON__CONFIG_FILE").ok() else {
        info!("TRITON__CONFIG_FILE not set; using default config");
        return Ok(ApiServerConfig::default());
    };

    let contents = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("failed to read config from {path}"))?;
    let config: ApiServerConfig = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse config from {path}"))?;
    info!("loaded config from {path}");
    Ok(config)
}

struct ApiContext {
    jwt: Option<Arc<JwtService>>,
    ldap: Option<Arc<LdapService>>,
    mahi: Option<Arc<MahiService>>,
    /// Whether to set the `Secure` flag on the auth cookie. Disabled for
    /// local HTTP development, enabled behind haproxy (the production
    /// deployment always terminates TLS in front of tritonapi).
    cookie_secure: bool,
    /// Persistence for Kelp cluster records. Always present — falls
    /// back to a file-backed store at `./data/clusters` when no
    /// `[clusters]` section is provided.
    cluster_store: Arc<dyn ClusterStore>,
    /// POC relay tunnel registry. Holds at most one agent tunnel at a time.
    relay: Arc<RelayState>,
    /// Operator CloudAPI client for server-side VM provisioning. `None` when
    /// no `[cloudapi]` section is in the config; bootstrap endpoint returns
    /// 503 in that case.
    cloudapi: Option<Arc<TypedClient>>,
    /// CloudAPI account login for the operator client (e.g. `"admin"`).
    cloudapi_account: Option<String>,
    /// Dev bypass: unauthenticated requests are treated as this account UUID.
    /// Never set in production.
    dev_account_uuid: Option<Uuid>,
}

enum TritonApiImpl {}

impl TritonApi for TritonApiImpl {
    type Context = ApiContext;

    async fn ping(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError> {
        Ok(HttpResponseOk(PingResponse {
            status: "OK".to_string(),
            healthy: Some(true),
        }))
    }

    async fn auth_login(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginRequest>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginOutcome>>, HttpError> {
        let ctx = rqctx.context();
        let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;
        let ldap = ctx.ldap.as_ref().ok_or_else(auth_unavailable)?;
        let mahi = ctx.mahi.as_ref().ok_or_else(auth_unavailable)?;

        let req = body.into_inner();
        let user = ldap
            .authenticate(&req.username, &req.password)
            .await
            .map_err(session_error_to_http)?;

        // 2FA gate: if the user has a TOTP secret stored under the
        // `portal/usemoresecurity` UFDS metadata key, hold off on
        // mahi + token issuance and return a challenge instead. The
        // verify endpoint will re-read the secret and finish the
        // session after the user proves possession of the code.
        if ldap
            .read_totp_secret(user.uuid)
            .await
            .map_err(session_error_to_http)?
            .is_some()
        {
            let challenge_token = jwt
                .create_challenge_token(user.uuid, &user.login)
                .map_err(session_error_to_http)?;
            // No cookie is set here — the session does not exist
            // yet. The cookie ships with the LoginResponse returned
            // by /v1/auth/login/verify.
            return Ok(HttpResponseHeaders::new_unnamed(HttpResponseOk(
                LoginOutcome::ChallengeRequired(LoginChallenge {
                    challenge_token,
                    methods: vec![ChallengeMethod::Totp],
                }),
            )));
        }

        // Password is verified; mahi now provides the canonical operator /
        // group view. A 404 here would mean mahi hasn't caught up with a
        // brand-new user yet; MahiService maps that to AuthenticationFailed
        // so the client can retry.
        let auth_info = mahi
            .lookup(&user.login)
            .await
            .map_err(session_error_to_http)?;
        issue_login_outcome(jwt, &auth_info, ctx.cookie_secure).await
    }

    // Operator runbook: lost authenticator
    // ------------------------------------
    // tritonapi only verifies in v1 -- enrollment and disable still
    // live in piranha. To unenroll a user who lost access to their
    // authenticator, clear the TOTP secret stored under
    // `metadata=portal, uuid=<USER_UUID>, ou=users, o=smartdc` --
    // either via the piranha "Disable two-factor" UI, or directly
    // from the headnode:
    //
    //   sdc-ufds search -s base \
    //     -b 'metadata=portal, uuid=<USER_UUID>, ou=users, o=smartdc' \
    //     '(objectclass=capimetadata)'
    //
    // Then either remove the `usemoresecurity` attribute or set its
    // `secretkey` field to an empty string. `read_totp_secret` treats
    // both as "not enrolled" and the user's next login skips the
    // challenge entirely.
    async fn auth_login_verify(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginVerifyRequest>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError> {
        let ctx = rqctx.context();
        let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;
        let ldap = ctx.ldap.as_ref().ok_or_else(auth_unavailable)?;
        let mahi = ctx.mahi.as_ref().ok_or_else(auth_unavailable)?;

        let req = body.into_inner();
        let claims = jwt
            .verify_challenge_token(&req.challenge_token)
            .map_err(session_error_to_http)?;

        // Re-read the TOTP secret on the server every time. The
        // challenge token deliberately does not carry the secret —
        // that would put it on the wire on every challenge issuance,
        // and it would also let an offline attacker who got both the
        // public verifier key and one challenge brute-force the
        // secret. Re-reading also means that if the user disabled
        // 2FA between login and verify, we fail closed rather than
        // letting them in without proving possession of the second
        // factor.
        let secret = ldap
            .read_totp_secret(claims.user_uuid())
            .await
            .map_err(session_error_to_http)?
            .ok_or(SessionError::AuthenticationFailed)
            .map_err(session_error_to_http)?;

        let valid = verify_totp(&secret, &req.code).map_err(session_error_to_http)?;
        if !valid {
            return Err(session_error_to_http(SessionError::AuthenticationFailed));
        }

        let auth_info = mahi
            .lookup(&claims.username)
            .await
            .map_err(session_error_to_http)?;
        issue_login_response(jwt, &auth_info, ctx.cookie_secure).await
    }

    async fn auth_login_ssh(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError> {
        let ctx = rqctx.context();
        let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;
        let mahi = ctx.mahi.as_ref().ok_or_else(auth_unavailable)?;

        // 1. Classify. This endpoint only accepts HTTP Signature -- Bearer
        //    wouldn't make sense (the whole point is bootstrapping a session
        //    from a fresh key), and unauthenticated requests must fail
        //    clearly rather than falling through to a misleading error.
        let auth_params = match auth_scheme::classify(rqctx.request.headers()) {
            auth_scheme::AuthScheme::HttpSignature(v) => v,
            auth_scheme::AuthScheme::Bearer(_) => {
                return Err(HttpError::for_client_error(
                    Some("WrongAuthScheme".to_string()),
                    dropshot::ClientErrorStatusCode::UNAUTHORIZED,
                    "this endpoint requires HTTP Signature auth; Bearer is \
                     not accepted on /v1/auth/login-ssh"
                        .to_string(),
                ));
            }
            auth_scheme::AuthScheme::None => {
                return Err(unauthorized());
            }
        };

        // 2. Parse the Authorization value.
        let parsed = http_sig::parse_signature_params(&auth_params)
            .map_err(|e| sig_parse_error(&e.to_string()))?;

        // 3. Parse keyId. Account-level (`/{account}/keys/{fp}`) and
        //    sub-user (`/{account}/users/{user}/keys/{fp}`) forms are
        //    both accepted; the branch affects which mahi record we
        //    read the key from and which principal the JWT identifies.
        let parsed_key_id = parse_key_id(&parsed.key_id)?;

        // 4. Clock-skew sanity check on the Date header. Signatures that
        //    are too stale or too far in the future are almost always a
        //    sign of a misconfigured client clock; surface that clearly
        //    rather than letting it look like a signature failure.
        check_clock_skew(rqctx.request.headers())?;

        // 5. Mahi lookup. Any failure here -- account or sub-user doesn't
        //    exist, mahi is unreachable, key not on this principal --
        //    collapses into the same opaque SignatureVerificationFailed
        //    so an attacker probing with arbitrary names can't
        //    distinguish "exists" from "doesn't".
        let auth_info = match &parsed_key_id.subuser {
            None => mahi
                .lookup(&parsed_key_id.account)
                .await
                .map_err(|_| sig_verify_failed())?,
            Some(user_login) => mahi
                .lookup_user(&parsed_key_id.account, user_login)
                .await
                .map_err(|_| sig_verify_failed())?,
        };
        let public_key = extract_public_key(&auth_info, &parsed_key_id)?;

        // 6. Reconstruct the signing string and verify.
        let path_and_query = rqctx
            .request
            .uri()
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());
        let signing_string = http_sig::build_signing_string(
            rqctx.request.method().as_str(),
            &path_and_query,
            rqctx.request.headers(),
            &parsed.headers,
        )
        .map_err(|e| sig_parse_error(&e.to_string()))?;
        http_sig::verify_signature(
            &public_key,
            &parsed.algorithm,
            signing_string.as_bytes(),
            &parsed.signature,
        )
        .map_err(|_| sig_verify_failed())?;

        // 7. Signature verified. Issue tokens for the matched principal.
        //    Account-level logins route through the shared
        //    `issue_login_response` tail that the password path uses;
        //    sub-user logins take a parallel path because their identity
        //    + role shape is different (no account.groups to derive
        //    operator status from, and user.roles is a list of uuids we
        //    don't currently resolve to names).
        match parsed_key_id.subuser {
            None => issue_login_response(jwt, &auth_info, ctx.cookie_secure).await,
            Some(_) => {
                let user = auth_info.user.as_ref().ok_or_else(sig_verify_failed)?;
                issue_subuser_login_response(jwt, user, ctx.cookie_secure).await
            }
        }
    }

    async fn auth_logout(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LogoutResponse>>, HttpError> {
        let ctx = rqctx.context();
        let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;

        // Logout must work for expired sessions too — otherwise users whose
        // session already timed out can never sign out of other devices.
        let token = extract_token(rqctx.request.headers()).ok_or_else(unauthorized)?;
        let claims = jwt
            .decode_ignoring_expiry(&token)
            .map_err(session_error_to_http)?;

        jwt.revoke_user_tokens(&claims.username).await;

        let cookie = build_auth_cookie("", 0, ctx.cookie_secure);
        let mut response =
            HttpResponseHeaders::new_unnamed(HttpResponseOk(LogoutResponse { ok: true }));
        set_cookie_header(response.headers_mut(), cookie)?;
        Ok(response)
    }

    async fn auth_refresh(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RefreshRequest>,
    ) -> Result<HttpResponseOk<RefreshResponse>, HttpError> {
        let ctx = rqctx.context();
        let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;

        let req = body.into_inner();
        let (token, refresh_token) = jwt
            .refresh(&req.refresh_token)
            .await
            .map_err(session_error_to_http)?;
        Ok(HttpResponseOk(RefreshResponse {
            token,
            refresh_token,
        }))
    }

    async fn auth_session(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<SessionResponse>, HttpError> {
        let ctx = rqctx.context();
        let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;

        let token = extract_token(rqctx.request.headers()).ok_or_else(unauthorized)?;
        let claims = jwt.verify_token(&token).map_err(session_error_to_http)?;

        Ok(HttpResponseOk(SessionResponse {
            // /v1/auth/session only has access to fields the JWT actually
            // carries (username, uuid, roles). email/name/company come
            // from UFDS at login time and are not cached in the token.
            user: UserInfo {
                id: claims.user_uuid(),
                username: claims.username.clone(),
                email: None,
                name: None,
                company: None,
                is_admin: claims.is_admin(),
            },
        }))
    }

    async fn auth_jwks(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<JwkSet>, HttpError> {
        let ctx = rqctx.context();
        let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;
        let jwks = jwt.jwks().map_err(session_error_to_http)?;
        Ok(HttpResponseOk(JwkSet {
            keys: jwks
                .keys
                .into_iter()
                .map(|k| Jwk {
                    kty: k.kty.to_string(),
                    crv: k.crv.to_string(),
                    alg: k.alg.to_string(),
                    key_use: k.key_use.to_string(),
                    kid: k.kid,
                    x: k.x,
                    y: k.y,
                })
                .collect(),
        }))
    }

    async fn k8s_clusters_create(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateClusterRequest>,
    ) -> Result<HttpResponseCreated<Cluster>, HttpError> {
        let caller = resolve_caller(&rqctx).await?;
        let req = body.into_inner();
        let record = ClusterRecord {
            id: Uuid::new_v4(),
            name: req.name,
            account_id: caller.account_id,
            state: ClusterState::Created,
            description: req.description,
            fabric_network_id: req.fabric_network_id,
            control_plane_config: None,
            worker_config: None,
            nodes: std::collections::HashMap::new(),
            last_fabric_ip_offset: None,
            talosconfig_yaml: None,
            kubeconfig_yaml: None,
            secrets_yaml: None,
            talos_ca_pem: None,
            talos_crt_pem: None,
            talos_key_pem: None,
            talos_version: None,
            created_at: chrono::Utc::now(),
        };
        rqctx
            .context()
            .cluster_store
            .create(&record)
            .await
            .map_err(store_error_to_http)?;
        Ok(HttpResponseCreated(Cluster::from(&record)))
    }

    async fn k8s_clusters_list(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<ClusterList>, HttpError> {
        let caller = resolve_caller(&rqctx).await?;
        let records = rqctx
            .context()
            .cluster_store
            .list_for_account(caller.account_id)
            .await
            .map_err(store_error_to_http)?;
        let items = records.iter().map(Cluster::from).collect();
        Ok(HttpResponseOk(ClusterList { items }))
    }

    async fn k8s_clusters_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
    ) -> Result<HttpResponseOk<Cluster>, HttpError> {
        let caller = resolve_caller(&rqctx).await?;
        let id = path.into_inner().cluster;
        let record = rqctx
            .context()
            .cluster_store
            .get(id)
            .await
            .map_err(store_error_to_http)?
            .ok_or_else(|| cluster_not_found(id))?;
        // Indistinguishable-not-found: a cluster owned by another
        // account looks identical to one that never existed, so a
        // caller probing arbitrary UUIDs can't enumerate other
        // accounts' resources.
        if record.account_id != caller.account_id {
            return Err(cluster_not_found(id));
        }
        Ok(HttpResponseOk(Cluster::from(&record)))
    }

    async fn k8s_clusters_delete(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let caller = resolve_caller(&rqctx).await?;
        let id = path.into_inner().cluster;
        let store = &rqctx.context().cluster_store;
        let record = store
            .get(id)
            .await
            .map_err(store_error_to_http)?
            .ok_or_else(|| cluster_not_found(id))?;
        if record.account_id != caller.account_id {
            return Err(cluster_not_found(id));
        }
        let removed = store.delete(id).await.map_err(store_error_to_http)?;
        if !removed {
            // Race: someone else deleted between get and delete.
            // Treat as already gone — the client's view (the cluster
            // is gone) is correct either way.
            return Err(cluster_not_found(id));
        }
        Ok(HttpResponseDeleted())
    }

    async fn k8s_cluster_kubeconfig(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
    ) -> Result<HttpResponseOk<KubeconfigResponse>, HttpError> {
        let caller = resolve_caller(&rqctx).await?;
        let id = path.into_inner().cluster;
        let record = rqctx
            .context()
            .cluster_store
            .get(id)
            .await
            .map_err(store_error_to_http)?
            .ok_or_else(|| cluster_not_found(id))?;
        if record.account_id != caller.account_id {
            return Err(cluster_not_found(id));
        }
        let kubeconfig = record.kubeconfig_yaml.ok_or_else(|| {
            HttpError::for_client_error(
                Some("NotFound".to_string()),
                ClientErrorStatusCode::NOT_FOUND,
                format!("kubeconfig for cluster {id} is not yet available"),
            )
        })?;
        Ok(HttpResponseOk(KubeconfigResponse { kubeconfig }))
    }

    async fn k8s_cluster_upgrade(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
        body: TypedBody<UpgradeClusterRequest>,
    ) -> Result<HttpResponseAccepted<Cluster>, HttpError> {
        let caller = resolve_caller(&rqctx).await?;
        let id = path.into_inner().cluster;
        let ctx = rqctx.context();
        let store = Arc::clone(&ctx.cluster_store);

        let record = store
            .get(id)
            .await
            .map_err(store_error_to_http)?
            .ok_or_else(|| cluster_not_found(id))?;
        if record.account_id != caller.account_id {
            return Err(cluster_not_found(id));
        }
        if record.state != ClusterState::Running {
            return Err(HttpError::for_client_error(
                Some("InvalidState".to_string()),
                ClientErrorStatusCode::CONFLICT,
                format!("cluster {id} must be in `running` state to upgrade"),
            ));
        }

        let req = body.into_inner();
        if req.talos_image.is_empty() {
            return Err(HttpError::for_client_error(
                Some("InvalidInput".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                "talos_image must not be empty".to_string(),
            ));
        }

        let relay = Arc::clone(&ctx.relay);
        let cluster_view = Cluster::from(&record);
        tokio::spawn(async move {
            if let Err(e) = run_upgrade(store, relay, record, req).await {
                tracing::error!(cluster = %id, error = %e, "upgrade failed");
            }
        });

        Ok(HttpResponseAccepted(cluster_view))
    }

    async fn k8s_cluster_nodes_add(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
        body: TypedBody<AddNodesRequest>,
    ) -> Result<HttpResponseAccepted<Cluster>, HttpError> {
        let caller = resolve_caller(&rqctx).await?;
        let id = path.into_inner().cluster;
        let ctx = rqctx.context();
        let store = Arc::clone(&ctx.cluster_store);

        let record = store
            .get(id)
            .await
            .map_err(store_error_to_http)?
            .ok_or_else(|| cluster_not_found(id))?;
        if record.account_id != caller.account_id {
            return Err(cluster_not_found(id));
        }
        if record.state != ClusterState::Running {
            return Err(HttpError::for_client_error(
                Some("InvalidState".to_string()),
                ClientErrorStatusCode::CONFLICT,
                format!("cluster {id} must be in `running` state to add nodes"),
            ));
        }

        let req = body.into_inner();
        if req.nodes.is_empty() {
            return Err(HttpError::for_client_error(
                Some("InvalidInput".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                "nodes list must not be empty".to_string(),
            ));
        }

        let relay = Arc::clone(&ctx.relay);
        let cluster_view = Cluster::from(&record);
        tokio::spawn(async move {
            if let Err(e) = run_add_nodes(store, relay, record, req.nodes).await {
                tracing::error!(cluster = %id, error = %e, "add-nodes failed");
            }
        });

        Ok(HttpResponseAccepted(cluster_view))
    }

    async fn k8s_cluster_bootstrap(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
        body: TypedBody<BootstrapClusterRequest>,
    ) -> Result<HttpResponseAccepted<Cluster>, HttpError> {
        let caller = resolve_caller(&rqctx).await?;
        let id = path.into_inner().cluster;
        let ctx = rqctx.context();
        let store = Arc::clone(&ctx.cluster_store);

        let mut record = store
            .get(id)
            .await
            .map_err(store_error_to_http)?
            .ok_or_else(|| cluster_not_found(id))?;
        if record.account_id != caller.account_id {
            return Err(cluster_not_found(id));
        }
        if record.state != ClusterState::Created {
            return Err(HttpError::for_client_error(
                Some("InvalidState".to_string()),
                ClientErrorStatusCode::CONFLICT,
                format!("cluster {id} must be in `created` state to bootstrap"),
            ));
        }

        let req = body.into_inner();
        if req.control_plane_count == 0 {
            return Err(HttpError::for_client_error(
                Some("InvalidInput".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                "control_plane_count must be at least 1".to_string(),
            ));
        }
        let fabric_network_id = record.fabric_network_id.ok_or_else(|| {
            HttpError::for_client_error(
                Some("InvalidInput".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                "cluster has no fabric_network_id; set one via update before bootstrapping"
                    .to_string(),
            )
        })?;
        let cloudapi = ctx.cloudapi.clone().ok_or_else(|| {
            HttpError::for_unavail(
                Some("ServiceUnavailable".to_string()),
                "CloudAPI operator client is not configured on this tritonapi instance".to_string(),
            )
        })?;
        let cloudapi_account = ctx
            .cloudapi_account
            .clone()
            .expect("cloudapi_account is always Some when cloudapi is Some");

        record.state = ClusterState::Provisioning;
        store.update(&record).await.map_err(store_error_to_http)?;

        let relay = Arc::clone(&ctx.relay);
        let cluster_view = Cluster::from(&record);
        tokio::spawn(async move {
            if let Err(e) = run_bootstrap(
                store,
                relay,
                cloudapi,
                cloudapi_account,
                fabric_network_id,
                record,
                req,
            )
            .await
            {
                tracing::error!(cluster = %id, error = %e, "bootstrap failed");
            }
        });

        Ok(HttpResponseAccepted(cluster_view))
    }

    async fn k8s_relay_register(
        rqctx: RequestContext<Self::Context>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult {
        let relay = Arc::clone(&rqctx.context().relay);

        // Wrap the raw HTTP-upgraded connection in a WebSocket frame layer,
        // then adapt that to the futures::io byte stream that yamux needs.
        let raw = upgraded.into_inner();
        let ws =
            tokio_tungstenite::WebSocketStream::from_raw_socket(raw, WsRole::Server, None).await;
        let ws_compat = WsCompat::new(ws);

        // The API server is the yamux CLIENT: it opens streams toward the agent.
        let conn = yamux::Connection::new(ws_compat, yamux::Config::default(), yamux::Mode::Client);

        let (tx, rx) = mpsc::channel(32);
        relay.register(relay::TunnelHandle { open_stream: tx });
        info!("relay agent registered");

        // Block until the agent disconnects; the driver clears the handle on exit.
        relay::run_agent_connection(conn, rx, Arc::clone(&relay)).await;

        info!("relay agent disconnected");
        Ok(())
    }

    async fn k8s_relay_connect(
        rqctx: RequestContext<Self::Context>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult {
        let relay = Arc::clone(&rqctx.context().relay);

        let raw = upgraded.into_inner();
        let ws =
            tokio_tungstenite::WebSocketStream::from_raw_socket(raw, WsRole::Server, None).await;
        let ws_compat = WsCompat::new(ws);

        // The API server is the yamux SERVER here: it accepts streams from the bridge.
        let mut conn =
            yamux::Connection::new(ws_compat, yamux::Config::default(), yamux::Mode::Server);

        loop {
            let stream = std::future::poll_fn(|cx| conn.poll_next_inbound(cx)).await;
            match stream {
                Some(Ok(bridge_stream)) => {
                    let relay = Arc::clone(&relay);
                    tokio::spawn(async move {
                        if let Err(e) = handle_bridge_stream(bridge_stream, relay).await {
                            warn!("bridge stream error: {e}");
                        }
                    });
                }
                Some(Err(e)) => {
                    warn!("yamux error on relay/connect: {e}");
                    break;
                }
                None => {
                    info!("bridge connection closed");
                    break;
                }
            }
        }

        Ok(())
    }
}

async fn handle_bridge_stream(
    mut bridge_stream: yamux::Stream,
    relay: Arc<RelayState>,
) -> anyhow::Result<()> {
    let target = read_connect_target(&mut bridge_stream).await?;
    info!("bridge stream requesting target: {target}");
    let mut agent_stream = relay.open_stream().await?;
    write_connect_target(&mut agent_stream, &target).await?;
    let mut bridge_compat = bridge_stream.compat();
    let mut agent_compat = agent_stream.compat();
    let (b_to_a, a_to_b) = bridge(&mut bridge_compat, &mut agent_compat).await?;
    info!("bridge to {target} closed: {b_to_a} B bridge→agent, {a_to_b} B agent→bridge");
    Ok(())
}

struct ProvisionedNode {
    fabric_ip: String,
    role: NodeRole,
}

/// Resolve an image name or UUID string to a CloudAPI image UUID.
async fn resolve_image_uuid(
    cloudapi: &TypedClient,
    account: &str,
    image: &str,
) -> anyhow::Result<Uuid> {
    if let Ok(uuid) = Uuid::parse_str(image) {
        return Ok(uuid);
    }
    let images = cloudapi
        .inner()
        .list_images()
        .account(account)
        .name(image)
        .send()
        .await
        .with_context(|| format!("list images with name={image}"))?
        .into_inner();
    images
        .into_iter()
        .next()
        .map(|img| img.id)
        .ok_or_else(|| anyhow::anyhow!("no image found with name {:?}", image))
}

/// Provision a single VM via CloudAPI and return its fabric IP.
async fn provision_vm(
    cloudapi: &TypedClient,
    account: &str,
    name: &str,
    image_uuid: Uuid,
    package: &str,
    fabric_network_id: Uuid,
) -> anyhow::Result<String> {
    use cloudapi_client::types::{CreateMachineRequest, MachineState, NetworkObject};

    let body = CreateMachineRequest {
        name: Some(name.to_string()),
        image: image_uuid,
        package: package.to_string(),
        networks: Some(vec![NetworkObject {
            ipv4_uuid: fabric_network_id,
            ipv4_ips: None,
            primary: Some(true),
        }]),
        affinity: None,
        locality: None,
        metadata: None,
        tags: None,
        firewall_enabled: None,
        deletion_protection: None,
        brand: None,
        volumes: None,
        disks: None,
        delegate_dataset: None,
        encrypted: None,
        allow_shared_images: None,
    };

    let machine = cloudapi
        .inner()
        .create_machine()
        .account(account)
        .body(body)
        .send()
        .await
        .with_context(|| format!("create machine {name}"))?
        .into_inner();

    let machine_id = machine.id;
    tracing::info!(name = %name, machine = %machine_id, "VM created, waiting for running");

    // Poll until running (max 10 minutes).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("timed out waiting for machine {machine_id} to reach running state");
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

        let m = cloudapi
            .inner()
            .get_machine()
            .account(account)
            .machine(machine_id)
            .send()
            .await
            .with_context(|| format!("poll machine {machine_id}"))?
            .into_inner();

        match m.state {
            MachineState::Running => break,
            MachineState::Failed => anyhow::bail!("machine {machine_id} entered failed state"),
            _ => {}
        }
    }
    tracing::info!(name = %name, machine = %machine_id, "VM running, fetching fabric NIC");

    // Find the NIC on the fabric network to get the fabric IP.
    let nics = cloudapi
        .inner()
        .list_nics()
        .account(account)
        .machine(machine_id)
        .send()
        .await
        .with_context(|| format!("list NICs for machine {machine_id}"))?
        .into_inner();

    nics.into_iter()
        .find(|n| n.network == fabric_network_id)
        .map(|n| n.ip)
        .ok_or_else(|| {
            anyhow::anyhow!("machine {machine_id} has no NIC on fabric network {fabric_network_id}")
        })
}

async fn run_bootstrap(
    store: Arc<dyn ClusterStore>,
    relay: Arc<RelayState>,
    cloudapi: Arc<TypedClient>,
    cloudapi_account: String,
    fabric_network_id: Uuid,
    mut record: ClusterRecord,
    req: BootstrapClusterRequest,
) -> anyhow::Result<()> {
    let cluster_id = record.id;
    let talos_version = req
        .talos_version
        .as_deref()
        .unwrap_or(talos_config::DEFAULT_TALOS_VERSION);
    let install_disk = req
        .install_disk
        .as_deref()
        .unwrap_or(talos_config::DEFAULT_INSTALL_DISK);

    // Phase 0: resolve image and provision VMs.
    let image_uuid = resolve_image_uuid(&cloudapi, &cloudapi_account, &req.image).await?;
    tracing::info!(cluster = %cluster_id, image = %image_uuid, "resolved image UUID");

    let mut provisioned: Vec<ProvisionedNode> = Vec::new();

    for i in 0..req.control_plane_count {
        let name = format!("{}-cp-{i}", record.name);
        let fabric_ip = provision_vm(
            &cloudapi,
            &cloudapi_account,
            &name,
            image_uuid,
            &req.package,
            fabric_network_id,
        )
        .await
        .with_context(|| format!("provision control-plane node {name}"))?;
        tracing::info!(cluster = %cluster_id, node = %name, ip = %fabric_ip, "control-plane VM ready");
        provisioned.push(ProvisionedNode {
            fabric_ip,
            role: NodeRole::Control,
        });
    }

    for i in 0..req.worker_count {
        let name = format!("{}-w-{i}", record.name);
        let fabric_ip = provision_vm(
            &cloudapi,
            &cloudapi_account,
            &name,
            image_uuid,
            &req.package,
            fabric_network_id,
        )
        .await
        .with_context(|| format!("provision worker node {name}"))?;
        tracing::info!(cluster = %cluster_id, node = %name, ip = %fabric_ip, "worker VM ready");
        provisioned.push(ProvisionedNode {
            fabric_ip,
            role: NodeRole::Worker,
        });
    }

    let first_cp_ip = provisioned
        .iter()
        .find(|n| n.role == NodeRole::Control)
        .map(|n| n.fabric_ip.clone())
        .expect("at least one control-plane node was provisioned");

    // Phase 1: generate Talos PKI and machine configs server-side.
    let secrets =
        talos_config::SecretsBundle::generate().context("generate Talos secrets bundle")?;
    let configs = talos_config::generate_machine_configs(
        &secrets,
        &record.name,
        &first_cp_ip,
        install_disk,
        talos_version,
    )
    .context("generate Talos machine configs")?;

    // Store secrets and PKI in the cluster record before applying configs.
    record.secrets_yaml =
        Some(serde_yaml::to_string(&secrets).context("serialize secrets bundle")?);
    record.talosconfig_yaml = Some(configs.talosconfig_yaml.clone());
    record.talos_ca_pem =
        Some(String::from_utf8(configs.ca_pem_raw.clone()).context("CA PEM utf8")?);
    record.talos_crt_pem =
        Some(String::from_utf8(configs.crt_pem_raw.clone()).context("crt PEM utf8")?);
    record.talos_key_pem =
        Some(String::from_utf8(configs.key_pem_raw.clone()).context("key PEM utf8")?);

    // Phase 2: apply machine configs to all nodes in maintenance mode.
    for node in &provisioned {
        let target = format!("{}:50000", node.fabric_ip);
        let machine_config = match node.role {
            NodeRole::Control => configs.controlplane_yaml.as_bytes().to_vec(),
            NodeRole::Worker => configs.worker_yaml.as_bytes().to_vec(),
        };
        let mut client = talos::TalosClient::connect_maintenance(Arc::clone(&relay), &target)
            .await
            .with_context(|| format!("maintenance connect to {target}"))?;
        client
            .apply_configuration(machine_config, true)
            .await
            .with_context(|| format!("apply config to {target}"))?;
        tracing::info!(cluster = %cluster_id, target = %target, "applied machine config");
    }

    // Record node inventory and persist (including generated PKI) before sleeping.
    for node in &provisioned {
        let node_id = Uuid::new_v4();
        record.nodes.insert(
            node_id.to_string(),
            NodeInfo {
                instance_id: node_id,
                primary_ip: node.fabric_ip.clone(),
                fabric_ip: node.fabric_ip.clone(),
                role: node.role,
            },
        );
    }
    store
        .update(&record)
        .await
        .with_context(|| format!("persist node inventory for cluster {cluster_id}"))?;

    // Phase 3: wait for nodes to reboot into full Talos mode.
    tracing::info!(cluster = %cluster_id, "waiting 90s for nodes to reboot");
    tokio::time::sleep(std::time::Duration::from_secs(90)).await;

    // Phase 4: bootstrap etcd on the first control-plane node.
    let cp_target = format!("{first_cp_ip}:50000");
    let mut client = talos::TalosClient::connect_authenticated(
        Arc::clone(&relay),
        &cp_target,
        &configs.ca_pem_raw,
        &configs.crt_pem_raw,
        &configs.key_pem_raw,
    )
    .await
    .with_context(|| format!("authenticated connect to {cp_target}"))?;
    client
        .bootstrap()
        .await
        .with_context(|| format!("bootstrap etcd on {cp_target}"))?;
    tracing::info!(cluster = %cluster_id, cp = %cp_target, "etcd bootstrapped");

    // Phase 5: wait for the Kubernetes API to come up.
    tracing::info!(cluster = %cluster_id, "waiting 120s for Kubernetes API");
    tokio::time::sleep(std::time::Duration::from_secs(120)).await;

    // Phase 6: retrieve kubeconfig.
    let mut client = talos::TalosClient::connect_authenticated(
        Arc::clone(&relay),
        &cp_target,
        &configs.ca_pem_raw,
        &configs.crt_pem_raw,
        &configs.key_pem_raw,
    )
    .await
    .with_context(|| format!("authenticated connect to {cp_target} for kubeconfig"))?;
    let kubeconfig_bytes = client.kubeconfig().await.context("retrieve kubeconfig")?;

    record.kubeconfig_yaml = Some(String::from_utf8_lossy(&kubeconfig_bytes).into_owned());
    record.state = ClusterState::Running;
    store
        .update(&record)
        .await
        .with_context(|| format!("update cluster {cluster_id} to Running"))?;
    tracing::info!(cluster = %cluster_id, "cluster bootstrap complete, state=Running");
    Ok(())
}

async fn run_upgrade(
    store: Arc<dyn ClusterStore>,
    relay: Arc<RelayState>,
    mut record: ClusterRecord,
    req: UpgradeClusterRequest,
) -> anyhow::Result<()> {
    let cluster_id = record.id;

    let ca_pem = record
        .talos_ca_pem
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cluster {cluster_id} has no stored Talos credentials"))?
        .as_bytes()
        .to_vec();
    let crt_pem = record
        .talos_crt_pem
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cluster {cluster_id} has no stored Talos credentials"))?
        .as_bytes()
        .to_vec();
    let key_pem = record
        .talos_key_pem
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cluster {cluster_id} has no stored Talos credentials"))?
        .as_bytes()
        .to_vec();

    // Control-plane nodes first (maintain etcd quorum), then workers.
    let targets: Vec<String> = record
        .nodes
        .values()
        .filter(|n| n.role == NodeRole::Control)
        .map(|n| format!("{}:50000", n.fabric_ip))
        .chain(
            record
                .nodes
                .values()
                .filter(|n| n.role == NodeRole::Worker)
                .map(|n| format!("{}:50000", n.fabric_ip)),
        )
        .collect();

    for target in &targets {
        tracing::info!(cluster = %cluster_id, target = %target, image = %req.talos_image, "upgrading node");
        let mut client = talos::TalosClient::connect_authenticated(
            Arc::clone(&relay),
            target,
            &ca_pem,
            &crt_pem,
            &key_pem,
        )
        .await
        .with_context(|| format!("authenticated connect to {target}"))?;
        client
            .upgrade(&req.talos_image, req.preserve)
            .await
            .with_context(|| format!("upgrade {target}"))?;
        tracing::info!(cluster = %cluster_id, target = %target, "upgrade triggered, waiting for reboot");
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
    }

    // Extract version from the image tag (e.g. "…:v1.8.0" → "1.8.0").
    let new_version = req
        .talos_image
        .rsplit(':')
        .next()
        .map(|v| v.trim_start_matches('v').to_string());

    record.talos_version = new_version.clone();
    if let (Some(cp), Some(v)) = (record.control_plane_config.as_mut(), new_version) {
        cp.talos_version = v;
    }

    store
        .update(&record)
        .await
        .with_context(|| format!("update cluster {cluster_id} after upgrade"))?;
    tracing::info!(cluster = %cluster_id, image = %req.talos_image, "cluster upgrade complete");
    Ok(())
}

async fn run_add_nodes(
    store: Arc<dyn ClusterStore>,
    relay: Arc<RelayState>,
    mut record: ClusterRecord,
    nodes: Vec<NodeBootstrapSpec>,
) -> anyhow::Result<()> {
    let cluster_id = record.id;

    // Reconstruct machine configs from the stored secrets bundle.
    let secrets_yaml = record
        .secrets_yaml
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cluster {cluster_id} has no stored secrets bundle"))?;
    let secrets: talos_config::SecretsBundle =
        serde_yaml::from_str(secrets_yaml).context("deserialize secrets bundle")?;

    let endpoint_ip = record
        .nodes
        .values()
        .find(|n| n.role == NodeRole::Control)
        .map(|n| n.fabric_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("cluster {cluster_id} has no control-plane node"))?;

    let configs = talos_config::generate_machine_configs(
        &secrets,
        &record.name,
        &endpoint_ip,
        talos_config::DEFAULT_INSTALL_DISK,
        talos_config::DEFAULT_TALOS_VERSION,
    )
    .context("generate machine configs for new nodes")?;

    for node in &nodes {
        let target = format!("{}:50000", node.fabric_ip);
        let machine_config = match node.role {
            NodeBootstrapRole::ControlPlane => configs.controlplane_yaml.as_bytes().to_vec(),
            NodeBootstrapRole::Worker => configs.worker_yaml.as_bytes().to_vec(),
        };
        let mut client = talos::TalosClient::connect_maintenance(Arc::clone(&relay), &target)
            .await
            .with_context(|| format!("maintenance connect to {target}"))?;
        client
            .apply_configuration(machine_config, true)
            .await
            .with_context(|| format!("apply config to {target}"))?;
        tracing::info!(cluster = %cluster_id, target = %target, "applied machine config to new node");
    }

    for node in &nodes {
        let role = match node.role {
            NodeBootstrapRole::ControlPlane => NodeRole::Control,
            NodeBootstrapRole::Worker => NodeRole::Worker,
        };
        let node_id = Uuid::new_v4();
        record.nodes.insert(
            node_id.to_string(),
            NodeInfo {
                instance_id: node_id,
                primary_ip: node.fabric_ip.clone(),
                fabric_ip: node.fabric_ip.clone(),
                role,
            },
        );
    }
    store
        .update(&record)
        .await
        .with_context(|| format!("persist new nodes for cluster {cluster_id}"))?;

    tracing::info!(
        cluster = %cluster_id,
        count = nodes.len(),
        "node configs applied; nodes will join cluster after reboot"
    );
    Ok(())
}

/// Authenticated caller identity for protected `/v1/k8s/*` endpoints.
struct CallerIdentity {
    /// UUID of the principal owning resources for this request. For
    /// Bearer JWTs this is `claims.sub` (account UUID for password /
    /// account-key logins, sub-user UUID for sub-user SSH logins
    /// per [`issue_subuser_login_response`]). For HTTP Signature auth
    /// this is `auth_info.account.uuid` for the account-level form
    /// or the sub-user's UUID for the sub-user form, matching the
    /// JWT semantics.
    account_id: Uuid,
}

/// Resolve the authenticated caller from a `/v1/k8s/*` request.
///
/// Accepts Bearer JWT (from the `Authorization` header or the `auth`
/// cookie) or HTTP Signature (the same `Authorization: Signature ...`
/// shape `/v1/auth/login-ssh` accepts). Unauthenticated requests
/// return 401; malformed credentials return 400 with the same error
/// codes the dedicated auth endpoints use, so client diagnostics
/// don't change shape between endpoints.
async fn resolve_caller(rqctx: &RequestContext<ApiContext>) -> Result<CallerIdentity, HttpError> {
    let ctx = rqctx.context();

    if let Some(uuid) = ctx.dev_account_uuid {
        return Ok(CallerIdentity { account_id: uuid });
    }

    let headers = rqctx.request.headers();

    match auth_scheme::classify(headers) {
        auth_scheme::AuthScheme::Bearer(token) => {
            let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;
            let claims = jwt.verify_token(&token).map_err(session_error_to_http)?;
            Ok(CallerIdentity {
                account_id: claims.user_uuid(),
            })
        }
        auth_scheme::AuthScheme::HttpSignature(auth_params) => {
            let mahi = ctx.mahi.as_ref().ok_or_else(auth_unavailable)?;
            let parsed = http_sig::parse_signature_params(&auth_params)
                .map_err(|e| sig_parse_error(&e.to_string()))?;
            let parsed_key_id = parse_key_id(&parsed.key_id)?;
            check_clock_skew(headers)?;

            // Same opaque-failure pattern as auth_login_ssh: don't let
            // a caller probing with arbitrary keyIds enumerate which
            // accounts / fingerprints exist.
            let auth_info = match &parsed_key_id.subuser {
                None => mahi
                    .lookup(&parsed_key_id.account)
                    .await
                    .map_err(|_| sig_verify_failed())?,
                Some(user_login) => mahi
                    .lookup_user(&parsed_key_id.account, user_login)
                    .await
                    .map_err(|_| sig_verify_failed())?,
            };
            let public_key = extract_public_key(&auth_info, &parsed_key_id)?;

            let path_and_query = rqctx
                .request
                .uri()
                .path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| "/".to_string());
            let signing_string = http_sig::build_signing_string(
                rqctx.request.method().as_str(),
                &path_and_query,
                headers,
                &parsed.headers,
            )
            .map_err(|e| sig_parse_error(&e.to_string()))?;
            http_sig::verify_signature(
                &public_key,
                &parsed.algorithm,
                signing_string.as_bytes(),
                &parsed.signature,
            )
            .map_err(|_| sig_verify_failed())?;

            let account_id = match &parsed_key_id.subuser {
                None => auth_info.account.uuid,
                Some(_) => auth_info.user.as_ref().ok_or_else(sig_verify_failed)?.uuid,
            };
            Ok(CallerIdentity { account_id })
        }
        auth_scheme::AuthScheme::None => Err(unauthorized()),
    }
}

fn cluster_not_found(id: Uuid) -> HttpError {
    HttpError::for_client_error(
        Some("NotFound".to_string()),
        ClientErrorStatusCode::NOT_FOUND,
        format!("cluster {id} not found"),
    )
}

fn store_error_to_http(err: StoreError) -> HttpError {
    match err {
        StoreError::AlreadyExists(id) => HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            format!("cluster {id} already exists"),
        ),
        StoreError::NotFound(id) => {
            HttpError::for_internal_error(format!("cluster {id} missing during update (race)"))
        }
        StoreError::Io(e) => HttpError::for_internal_error(format!("cluster store I/O error: {e}")),
        StoreError::Serialize(e) => {
            HttpError::for_internal_error(format!("cluster store serialization error: {e}"))
        }
    }
}

fn build_auth_cookie(token: &str, max_age: u64, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!("auth={token}; HttpOnly{secure_flag}; SameSite=Strict; Path=/; Max-Age={max_age}")
}

fn set_cookie_header(headers: &mut http::HeaderMap, cookie: String) -> Result<(), HttpError> {
    let value = http::HeaderValue::from_str(&cookie).map_err(|e| {
        HttpError::for_internal_error(format!("failed to build Set-Cookie header: {e}"))
    })?;
    headers.insert(http::header::SET_COOKIE, value);
    Ok(())
}

/// Pull the bearer token from either `Authorization: Bearer ...` or the
/// `auth` cookie, in that order. Browsers use the cookie; CLIs use the
/// Authorization header.
fn extract_token(headers: &http::HeaderMap) -> Option<String> {
    if let Some(auth) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(token) = auth.strip_prefix("Bearer ")
    {
        return Some(token.to_string());
    }

    if let Some(cookie_header) = headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        for part in cookie_header.split(';') {
            if let Some(value) = part.trim().strip_prefix("auth=") {
                return Some(value.to_string());
            }
        }
    }

    None
}

fn session_error_to_http(err: SessionError) -> HttpError {
    match err {
        SessionError::AuthenticationFailed
        | SessionError::InvalidToken
        | SessionError::TokenExpired => HttpError::for_client_error(
            Some("Unauthorized".to_string()),
            ClientErrorStatusCode::UNAUTHORIZED,
            err.to_string(),
        ),
        SessionError::LdapUnavailable(msg) | SessionError::MahiUnavailable(msg) => {
            HttpError::for_unavail(Some("ServiceUnavailable".to_string()), msg)
        }
        SessionError::LdapConfigError(msg) | SessionError::JwtKeyError(msg) => {
            HttpError::for_internal_error(msg)
        }
        SessionError::Internal(msg) => HttpError::for_internal_error(msg),
    }
}

fn unauthorized() -> HttpError {
    HttpError::for_client_error(
        Some("Unauthorized".to_string()),
        ClientErrorStatusCode::UNAUTHORIZED,
        "missing or malformed authentication token".to_string(),
    )
}

fn auth_unavailable() -> HttpError {
    HttpError::for_unavail(
        Some("ServiceUnavailable".to_string()),
        "authentication is not configured on this tritonapi instance".to_string(),
    )
}

/// Collapse every "I can't verify this signature" failure mode --
/// account missing from mahi, fingerprint not on the account, crypto
/// rejected it, OpenSSH blob unparseable -- into one opaque 401 so an
/// attacker probing with arbitrary keyIds can't enumerate accounts or
/// distinguish "account doesn't exist" from "wrong key".
fn sig_verify_failed() -> HttpError {
    HttpError::for_client_error(
        Some("SignatureVerificationFailed".to_string()),
        ClientErrorStatusCode::UNAUTHORIZED,
        "signature verification failed".to_string(),
    )
}

/// Parser-level failure (malformed Authorization value, unsupported
/// algorithm, missing header referenced by the signature). These are
/// client bugs rather than auth attempts, so they get a 400 with a
/// specific message.
fn sig_parse_error(detail: &str) -> HttpError {
    HttpError::for_client_error(
        Some("MalformedSignature".to_string()),
        ClientErrorStatusCode::BAD_REQUEST,
        format!("invalid HTTP Signature: {detail}"),
    )
}

/// Parsed keyId identifying which principal signed a login-ssh request.
///
/// `subuser` is `Some` for the sub-user form
/// (`/{account}/users/{user}/keys/{fp}`) and `None` for the account-level
/// form (`/{account}/keys/{fp}`). The caller branches on this to pick
/// the right mahi lookup and JWT-claims shape.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedKeyId {
    account: String,
    subuser: Option<String>,
    fingerprint: String,
}

/// Split a draft-cavage keyId into `(account, subuser?, fingerprint)`.
///
/// Accepts either `/{account}/keys/{fp}` or
/// `/{account}/users/{user}/keys/{fp}`. Both forms may omit the leading
/// slash (some signers do). Any other shape is a 400 `MalformedKeyId`.
fn parse_key_id(key_id: &str) -> Result<ParsedKeyId, HttpError> {
    // Can't just `split('/')` -- SHA256 fingerprints are
    // `SHA256:<base64>` and the base64 alphabet includes `/`, so the
    // fingerprint itself may contain slashes. Locate the `/keys/`
    // separator explicitly; everything after it is the opaque
    // fingerprint, everything before it is either `{account}` or
    // `{account}/users/{user}`.
    let malformed = || {
        HttpError::for_client_error(
            Some("MalformedKeyId".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            format!(
                "keyId must be /{{account}}/keys/{{fingerprint}} or \
                 /{{account}}/users/{{user}}/keys/{{fingerprint}}, got: {key_id}"
            ),
        )
    };
    let stripped = key_id.strip_prefix('/').unwrap_or(key_id);
    let keys_pos = stripped.find("/keys/").ok_or_else(malformed)?;
    let (prefix, rest) = stripped.split_at(keys_pos);
    let fingerprint = &rest["/keys/".len()..];
    if fingerprint.is_empty() {
        return Err(malformed());
    }
    let prefix_parts: Vec<&str> = prefix.split('/').collect();
    match prefix_parts.as_slice() {
        [account] if !account.is_empty() => Ok(ParsedKeyId {
            account: (*account).to_string(),
            subuser: None,
            fingerprint: fingerprint.to_string(),
        }),
        [account, "users", user] if !account.is_empty() && !user.is_empty() => Ok(ParsedKeyId {
            account: (*account).to_string(),
            subuser: Some((*user).to_string()),
            fingerprint: fingerprint.to_string(),
        }),
        _ => Err(malformed()),
    }
}

/// Upper bound on `Date` header skew (each direction). Five minutes
/// matches the cloudapi/restify convention and is generous enough to
/// tolerate typical client clock drift without letting replay attempts
/// run indefinitely.
const DATE_SKEW_WINDOW_SECS: i64 = 300;

fn check_clock_skew(headers: &http::HeaderMap) -> Result<(), HttpError> {
    let Some(raw) = headers
        .get(http::header::DATE)
        .and_then(|v| v.to_str().ok())
    else {
        // The signing string almost always includes `date`, so a missing
        // Date header would fail later at signing-string construction
        // anyway -- but surface the condition clearly here rather than
        // relying on that downstream error.
        return Err(HttpError::for_client_error(
            Some("MissingDateHeader".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "request is missing a Date header".to_string(),
        ));
    };
    let parsed = chrono::DateTime::parse_from_rfc2822(raw).map_err(|_| {
        HttpError::for_client_error(
            Some("MalformedDateHeader".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "Date header is not RFC 2822 format".to_string(),
        )
    })?;
    let skew = chrono::Utc::now()
        .signed_duration_since(parsed.with_timezone(&chrono::Utc))
        .num_seconds();
    if skew.abs() > DATE_SKEW_WINDOW_SECS {
        return Err(HttpError::for_client_error(
            Some("ClockSkew".to_string()),
            ClientErrorStatusCode::UNAUTHORIZED,
            format!(
                "Date header differs from server time by {skew}s (allowed: \
                 ±{DATE_SKEW_WINDOW_SECS}s) -- check client clock"
            ),
        ));
    }
    Ok(())
}

/// Pull the signing public key out of a mahi `AuthInfo`, choosing the
/// right source record based on whether the keyId named an account-level
/// or sub-user principal.
///
/// Mahi stores `keys` as `fingerprint -> blob` on both account and user
/// records (since commit acfbaad made the field explicit on the mahi-api
/// `User` schema so Progenitor preserves it). Any failure -- missing
/// record, missing key, non-string blob, unparseable blob -- collapses
/// to the opaque `SignatureVerificationFailed` so probing can't
/// enumerate which fingerprints exist on which principals.
fn extract_public_key(
    auth_info: &triton_auth_session::AuthInfo,
    parsed_key_id: &ParsedKeyId,
) -> Result<http_sig::PublicKey, HttpError> {
    let blob = match &parsed_key_id.subuser {
        None => auth_info
            .account
            .keys
            .as_ref()
            .and_then(|keys| keys.get(&parsed_key_id.fingerprint))
            .and_then(|v| v.as_str())
            .ok_or_else(sig_verify_failed)?
            .to_string(),
        Some(_) => {
            let user = auth_info.user.as_ref().ok_or_else(sig_verify_failed)?;
            user.keys
                .as_ref()
                .and_then(|keys| keys.get(&parsed_key_id.fingerprint))
                .and_then(|v| v.as_str())
                .ok_or_else(sig_verify_failed)?
                .to_string()
        }
    };
    http_sig::parse_public_key_blob(&blob).map_err(|_| sig_verify_failed())
}

/// Issue tokens for a verified sub-user SSH login. Parallel to
/// [`issue_login_response`] but keyed on the mahi `User` record rather
/// than the `Account`: the JWT's `sub` carries the sub-user's uuid and
/// `username` carries the sub-user's login, so downstream authorization
/// sees the sub-user identity.
///
/// Roles are always empty and `is_admin` is always false: mahi models
/// sub-user roles as a list of role uuids (not group names), and
/// resolving those to the `Role` enum shape the JWT expects needs
/// additional mahi plumbing that will land in a follow-up slice when
/// we add real sub-user RBAC. Until then, a sub-user session is
/// "authenticated but unprivileged"; CloudAPI behind the gateway is
/// the ultimate authorization check regardless.
async fn issue_subuser_login_response(
    jwt: &JwtService,
    user: &triton_auth_session::mahi::User,
    cookie_secure: bool,
) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError> {
    let roles: Vec<Role> = Vec::new();
    let token = jwt
        .create_token(user.uuid, &user.login, &roles)
        .map_err(session_error_to_http)?;
    let refresh_token = jwt
        .create_refresh_token(user.uuid, &user.login, &roles)
        .await;

    let user_info = UserInfo {
        id: user.uuid,
        username: user.login.clone(),
        email: user.email.clone(),
        name: user.cn.clone(),
        company: user.company.clone(),
        is_admin: false,
    };

    let cookie = build_auth_cookie(&token, jwt.access_ttl_secs(), cookie_secure);
    let mut response = HttpResponseHeaders::new_unnamed(HttpResponseOk(LoginResponse {
        token,
        refresh_token,
        user: user_info,
    }));
    set_cookie_header(response.headers_mut(), cookie)?;
    Ok(response)
}

/// Build the `LoginResponse` body for a verified mahi account: mint
/// access + refresh tokens, derive `is_admin` from group / operator
/// status, and assemble `UserInfo`. Shared by both response wrappers
/// below; the wrappers are the only places that decide which outer
/// shape (`LoginResponse` vs `LoginOutcome::Complete`) and which
/// cookie behaviour is appropriate for the caller.
async fn build_login_response(
    jwt: &JwtService,
    auth_info: &triton_auth_session::AuthInfo,
) -> Result<LoginResponse, HttpError> {
    let account = &auth_info.account;
    let roles: Vec<Role> = account
        .groups
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|g| Role::from(g.as_str()))
        .collect();
    let is_operator = account.is_operator.unwrap_or(false);

    let token = jwt
        .create_token(account.uuid, &account.login, &roles)
        .map_err(session_error_to_http)?;
    let refresh_token = jwt
        .create_refresh_token(account.uuid, &account.login, &roles)
        .await;

    let is_admin = is_operator || triton_auth_session::roles_imply_admin(&roles);
    let user_info = UserInfo {
        id: account.uuid,
        username: account.login.clone(),
        email: account.email.clone(),
        name: account.cn.clone(),
        company: account.company.clone(),
        is_admin,
    };

    Ok(LoginResponse {
        token,
        refresh_token,
        user: user_info,
    })
}

/// Wrap [`build_login_response`] in `HttpResponseHeaders<HttpResponseOk<LoginResponse>>`
/// with the auth cookie set. Used by the SSH-login path and by the
/// 2FA verify path — both yield a flat `LoginResponse`.
async fn issue_login_response(
    jwt: &JwtService,
    auth_info: &triton_auth_session::AuthInfo,
    cookie_secure: bool,
) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError> {
    let body = build_login_response(jwt, auth_info).await?;
    let cookie = build_auth_cookie(&body.token, jwt.access_ttl_secs(), cookie_secure);
    let mut response = HttpResponseHeaders::new_unnamed(HttpResponseOk(body));
    set_cookie_header(response.headers_mut(), cookie)?;
    Ok(response)
}

/// Wrap [`build_login_response`] in `LoginOutcome::Complete` for the
/// password-login path. Same cookie behaviour as
/// [`issue_login_response`]; only the outer wire shape differs.
async fn issue_login_outcome(
    jwt: &JwtService,
    auth_info: &triton_auth_session::AuthInfo,
    cookie_secure: bool,
) -> Result<HttpResponseHeaders<HttpResponseOk<LoginOutcome>>, HttpError> {
    let body = build_login_response(jwt, auth_info).await?;
    let cookie = build_auth_cookie(&body.token, jwt.access_ttl_secs(), cookie_secure);
    let mut response =
        HttpResponseHeaders::new_unnamed(HttpResponseOk(LoginOutcome::Complete(body)));
    set_cookie_header(response.headers_mut(), cookie)?;
    Ok(response)
}

async fn build_jwt_service(cfg: &JwtConfigFile) -> Result<JwtService> {
    let private_pem = tokio::fs::read_to_string(&cfg.private_key_file)
        .await
        .with_context(|| format!("read JWT private key from {}", cfg.private_key_file))?;
    let public_pem = tokio::fs::read_to_string(&cfg.public_key_file)
        .await
        .with_context(|| format!("read JWT public key from {}", cfg.public_key_file))?;

    // Stable kid derived from the public key so it remains consistent
    // across service restarts without needing an additional config field.
    let kid = derive_kid(&public_pem);

    let session_cfg = SessionJwtConfig {
        private_key_pem: SecretString::new(private_pem.into()),
        public_key_pem: public_pem,
        kid,
        access_ttl_secs: cfg.access_ttl_secs,
        refresh_ttl_secs: cfg.refresh_ttl_secs,
    };
    JwtService::new(&session_cfg).context("construct JwtService")
}

fn derive_kid(public_pem: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    public_pem.hash(&mut hasher);
    format!("jwt-{:x}", hasher.finish())
}

fn build_ldap_service(cfg: &LdapConfigFile) -> LdapService {
    LdapService::new(SessionLdapConfig {
        url: cfg.url.clone(),
        bind_dn: cfg.bind_dn.clone(),
        bind_password: cfg.bind_password.clone(),
        search_base: cfg.search_base.clone(),
        tls_verify: cfg.tls_verify,
        connection_timeout_secs: cfg.connection_timeout_secs,
    })
}

async fn build_cloudapi_client(
    cfg: Option<&CloudApiConfigFile>,
) -> Result<Option<Arc<TypedClient>>> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };

    let key_source = match cfg.key_fingerprint.as_deref() {
        Some(_fp) => KeySource::file(&cfg.key_file),
        None => KeySource::file(&cfg.key_file),
    };
    let auth_config = AuthConfig::new(&cfg.account, key_source);

    // Use the shared TLS client builder so the server survives on zones
    // whose native CA store is empty (the relay zone falls into this category).
    let http_client = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client for CloudAPI")?;

    let client = TypedClient::new_with_http_client(cfg.url.as_str(), auth_config, http_client);
    info!(
        "CloudAPI operator client: {} account={}",
        cfg.url, cfg.account
    );
    Ok(Some(Arc::new(client)))
}

async fn build_mahi_service(cfg: &MahiConfigFile) -> Result<MahiService> {
    // Use triton-tls's client builder so the service survives on zones
    // whose native CA store is empty (reqwest's default builder panics
    // there). Mahi speaks plain HTTP today, but going through build_http_client
    // keeps us consistent with the other admin-plane clients.
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client for mahi")?;
    Ok(MahiService::new(cfg.url.as_str(), http))
}

fn version_string() -> &'static str {
    concat!(
        "triton-api-server ",
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("GIT_COMMIT_SHORT"),
        env!("GIT_DIRTY_SUFFIX"),
        ")"
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("version") {
        println!("{}", version_string());
        return Ok(());
    }

    // Install the rustls crypto provider before `mahi-client::Client::new`
    // (and anything else that builds a reqwest or rustls client) runs.
    // `triton-tls` owns backend selection.
    triton_tls::install_default_crypto_provider();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "triton_api_server=info,triton_auth_session=debug,dropshot=info",
        ))
        .init();

    let config = load_config().await?;

    let jwt = match config.jwt.as_ref() {
        Some(cfg) => Some(Arc::new(build_jwt_service(cfg).await?)),
        None => {
            warn!("no [jwt] section in config; /v1/auth/* endpoints will return 503");
            None
        }
    };
    let ldap = match config.ldap.as_ref() {
        Some(cfg) => Some(Arc::new(build_ldap_service(cfg))),
        None => {
            warn!("no [ldap] section in config; /v1/auth/login will return 503");
            None
        }
    };
    let mahi = match config.mahi.as_ref() {
        Some(cfg) => Some(Arc::new(build_mahi_service(cfg).await?)),
        None => {
            warn!("no [mahi] section in config; /v1/auth/login will return 503");
            None
        }
    };

    let api = triton_api::triton_api_mod::api_description::<TritonApiImpl>()
        .map_err(|e| anyhow::anyhow!("Failed to create API description: {}", e))?;

    let max_body_bytes_u64 = config.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES);
    let max_body_bytes: usize = usize::try_from(max_body_bytes_u64).with_context(|| {
        format!("max_body_bytes {max_body_bytes_u64} does not fit in usize on this platform")
    })?;
    info!("request body size limit: {max_body_bytes} bytes");

    let config_dropshot = ConfigDropshot {
        bind_address: config.bind_address.parse()?,
        default_request_body_max_bytes: max_body_bytes,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };

    let log = config_logging
        .to_logger("triton-api-server")
        .map_err(|error| anyhow::anyhow!("failed to create logger: {}", error))?;

    // Behind haproxy the deployed zone always terminates TLS up front, so
    // cookies always get Secure there. Local dev (no TLS terminator) binds
    // loopback only, so turning it off there isn't a security hole.
    let cookie_secure = true;

    let cluster_store: Arc<dyn ClusterStore> = Arc::new(
        FileClusterStore::new(config.clusters.state_dir.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to initialize cluster store at {}",
                    config.clusters.state_dir.display()
                )
            })?,
    );
    info!(
        "cluster store: file-backed at {}",
        config.clusters.state_dir.display()
    );

    let cloudapi = build_cloudapi_client(config.cloudapi.as_ref()).await?;
    if cloudapi.is_none() {
        warn!("no [cloudapi] section in config; bootstrap endpoint will return 503");
    }

    if config.dev_account_uuid.is_some() {
        warn!(
            "dev_account_uuid is set; unauthenticated requests will bypass auth — do not use in production"
        );
    }

    let context = ApiContext {
        jwt,
        ldap,
        mahi,
        cookie_secure,
        cluster_store,
        relay: Arc::new(relay::RelayState::new()),
        cloudapi,
        cloudapi_account: config.cloudapi.as_ref().map(|c| c.account.clone()),
        dev_account_uuid: config.dev_account_uuid,
    };

    let server = HttpServerStarter::new(&config_dropshot, api, context, &log)
        .map_err(|error| anyhow::anyhow!("failed to create server: {}", error))?
        .start();

    info!(
        "triton-api-server listening on http://{}",
        config.bind_address
    );

    tokio::select! {
        result = server.wait_for_shutdown() => {
            return result.map_err(|error| anyhow::anyhow!("server failed: {}", error));
        }
        () = shutdown_signal() => {}
    }

    server
        .close()
        .await
        .map_err(|error| anyhow::anyhow!("graceful shutdown failed: {}", error))
}

/// Await either SIGTERM or SIGINT.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).ok();
    let sigterm_fut = async {
        match sigterm.as_mut() {
            Some(s) => {
                s.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm_fut => {},
    }
    info!("shutdown signal received, draining in-flight requests");
}

#[cfg(test)]
mod k8s_helper_tests {
    //! Tests for the helpers the /v1/k8s/clusters/* handlers layer on
    //! top of `cluster_store` and the shared auth resolver. The store
    //! itself has direct coverage in `cluster_store::tests`; the auth
    //! resolver delegates to primitives that are tested in
    //! `libs/triton-auth` and `libs/triton-auth-session`. Full HTTP
    //! integration tests are deferred until triton-api-server is
    //! restructured into a lib + thin binary so a test can drive the
    //! Dropshot server with a real `ApiContext`; see
    //! `docs/design/kelp-cluster-storage.md`.
    use super::*;

    #[test]
    fn cluster_not_found_uses_404() {
        let id = Uuid::new_v4();
        let err = cluster_not_found(id);
        assert_eq!(err.error_code.as_deref(), Some("NotFound"));
        assert_eq!(
            err.status_code,
            dropshot::ErrorStatusCode::from(ClientErrorStatusCode::NOT_FOUND)
        );
        assert!(err.external_message.contains(&id.to_string()));
    }

    #[test]
    fn store_error_already_exists_maps_to_409() {
        let id = Uuid::new_v4();
        let err = store_error_to_http(StoreError::AlreadyExists(id));
        assert_eq!(err.error_code.as_deref(), Some("Conflict"));
        assert_eq!(
            err.status_code,
            dropshot::ErrorStatusCode::from(ClientErrorStatusCode::CONFLICT)
        );
    }

    #[test]
    fn store_error_not_found_maps_to_500() {
        let id = Uuid::new_v4();
        let err = store_error_to_http(StoreError::NotFound(id));
        assert!(err.status_code.as_u16() >= 500);
    }

    #[test]
    fn store_error_io_maps_to_500() {
        let io = std::io::Error::other("disk full");
        let err = store_error_to_http(StoreError::Io(io));
        // Internal errors don't expose error_code on the wire; only the
        // status matters here.
        assert!(err.status_code.as_u16() >= 500);
    }
}

#[cfg(test)]
mod login_ssh_helper_tests {
    //! Coverage for the helpers the /v1/auth/login-ssh handler layers
    //! on top of the classifier + verifier: keyId parsing, clock-skew
    //! enforcement, OpenSSH blob extraction. End-to-end handler tests
    //! need mahi+jwt mocking and land separately.
    use super::*;

    #[test]
    fn parse_key_id_account_form_ok() {
        let parsed = parse_key_id("/admin/keys/0f:7d:59:bc").unwrap();
        assert_eq!(parsed.account, "admin");
        assert_eq!(parsed.subuser, None);
        assert_eq!(parsed.fingerprint, "0f:7d:59:bc");
    }

    #[test]
    fn parse_key_id_accepts_sha256_fingerprint() {
        let parsed =
            parse_key_id("/admin/keys/SHA256:29ZAWD34TsVSP+FfrqK776oo6FcrOg+Ysp/ZVLNAZRA").unwrap();
        assert_eq!(parsed.account, "admin");
        assert_eq!(parsed.subuser, None);
        assert_eq!(
            parsed.fingerprint,
            "SHA256:29ZAWD34TsVSP+FfrqK776oo6FcrOg+Ysp/ZVLNAZRA"
        );
    }

    #[test]
    fn parse_key_id_subuser_form_ok() {
        let parsed = parse_key_id("/admin/users/bob/keys/0f:7d:59:bc").unwrap();
        assert_eq!(parsed.account, "admin");
        assert_eq!(parsed.subuser.as_deref(), Some("bob"));
        assert_eq!(parsed.fingerprint, "0f:7d:59:bc");
    }

    #[test]
    fn parse_key_id_subuser_form_accepts_sha256_fingerprint() {
        // Regression guard against re-introducing a split-on-`/` parser
        // that would misread a SHA256 fingerprint as extra path segments.
        let parsed = parse_key_id(
            "/admin/users/bob/keys/SHA256:29ZAWD34TsVSP+FfrqK776oo6FcrOg+Ysp/ZVLNAZRA",
        )
        .unwrap();
        assert_eq!(parsed.account, "admin");
        assert_eq!(parsed.subuser.as_deref(), Some("bob"));
        assert_eq!(
            parsed.fingerprint,
            "SHA256:29ZAWD34TsVSP+FfrqK776oo6FcrOg+Ysp/ZVLNAZRA"
        );
    }

    #[test]
    fn parse_key_id_malformed_missing_keys_segment() {
        let err = parse_key_id("/admin/0f:7d").unwrap_err();
        assert_eq!(err.error_code.as_deref(), Some("MalformedKeyId"));
    }

    #[test]
    fn parse_key_id_malformed_empty_account() {
        let err = parse_key_id("//keys/0f:7d").unwrap_err();
        assert_eq!(err.error_code.as_deref(), Some("MalformedKeyId"));
    }

    #[test]
    fn parse_key_id_malformed_empty_subuser() {
        // `/admin/users//keys/fp` matches the sub-user *shape* but an
        // empty sub-user login is never valid.
        let err = parse_key_id("/admin/users//keys/0f:7d").unwrap_err();
        assert_eq!(err.error_code.as_deref(), Some("MalformedKeyId"));
    }

    #[test]
    fn parse_key_id_malformed_empty_fingerprint() {
        let err = parse_key_id("/admin/keys/").unwrap_err();
        assert_eq!(err.error_code.as_deref(), Some("MalformedKeyId"));
    }

    #[test]
    fn parse_key_id_accepts_no_leading_slash() {
        // Some clients omit the leading slash; accept both forms since
        // the split-and-match logic handles either identically.
        let parsed = parse_key_id("admin/keys/abc").unwrap();
        assert_eq!(parsed.account, "admin");
        assert_eq!(parsed.subuser, None);
        assert_eq!(parsed.fingerprint, "abc");

        let parsed = parse_key_id("admin/users/bob/keys/abc").unwrap();
        assert_eq!(parsed.account, "admin");
        assert_eq!(parsed.subuser.as_deref(), Some("bob"));
        assert_eq!(parsed.fingerprint, "abc");
    }

    fn headers_with_date(value: &str) -> http::HeaderMap {
        let mut h = http::HeaderMap::new();
        h.insert(
            http::header::DATE,
            http::HeaderValue::from_str(value).unwrap(),
        );
        h
    }

    #[test]
    fn clock_skew_missing_date_header() {
        let err = check_clock_skew(&http::HeaderMap::new()).unwrap_err();
        assert_eq!(err.error_code.as_deref(), Some("MissingDateHeader"));
    }

    #[test]
    fn clock_skew_malformed_date() {
        let err = check_clock_skew(&headers_with_date("not a date")).unwrap_err();
        assert_eq!(err.error_code.as_deref(), Some("MalformedDateHeader"));
    }

    #[test]
    fn clock_skew_now_accepted() {
        // `chrono::Utc::now()` formatted as RFC 2822 is always within
        // the window regardless of wall time.
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        assert!(check_clock_skew(&headers_with_date(&now)).is_ok());
    }

    #[test]
    fn clock_skew_too_old_rejected() {
        let stale = (chrono::Utc::now() - chrono::Duration::seconds(DATE_SKEW_WINDOW_SECS + 60))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let err = check_clock_skew(&headers_with_date(&stale)).unwrap_err();
        assert_eq!(err.error_code.as_deref(), Some("ClockSkew"));
    }

    #[test]
    fn clock_skew_too_far_future_rejected() {
        let future = (chrono::Utc::now() + chrono::Duration::seconds(DATE_SKEW_WINDOW_SECS + 60))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let err = check_clock_skew(&headers_with_date(&future)).unwrap_err();
        assert_eq!(err.error_code.as_deref(), Some("ClockSkew"));
    }

    #[test]
    fn clock_skew_within_window_accepted() {
        // Just inside the window in both directions -- exercise the
        // boundary, not just the happy middle.
        for offset in [-DATE_SKEW_WINDOW_SECS + 5, DATE_SKEW_WINDOW_SECS - 5] {
            let d = (chrono::Utc::now() + chrono::Duration::seconds(offset))
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string();
            assert!(
                check_clock_skew(&headers_with_date(&d)).is_ok(),
                "offset {offset}s should be within window"
            );
        }
    }
}
