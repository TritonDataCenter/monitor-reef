// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::{Context, Result};
use dropshot::{
    ClientErrorStatusCode, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError,
    HttpResponseHeaders, HttpResponseOk, HttpServerStarter, RequestContext, TypedBody,
};
use secrecy::SecretString;
use serde::Deserialize;
use std::num::NonZeroU64;
use std::sync::Arc;
use tracing::{info, warn};
use triton_api::{
    Jwk, JwkSet, LoginRequest, LoginResponse, LogoutResponse, PingResponse, RefreshRequest,
    RefreshResponse, SessionResponse, TritonApi, UserInfo,
};
use triton_auth_session::{
    JwtConfig as SessionJwtConfig, JwtService, LdapConfig as SessionLdapConfig, LdapService,
    SessionError,
};

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
    jwt: Option<JwtConfigFile>,
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
            jwt: None,
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
    /// Whether to set the `Secure` flag on the auth cookie. Disabled for
    /// local HTTP development, enabled behind haproxy (the production
    /// deployment always terminates TLS in front of tritonapi).
    cookie_secure: bool,
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
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError> {
        let ctx = rqctx.context();
        let jwt = ctx.jwt.as_ref().ok_or_else(auth_unavailable)?;
        let ldap = ctx.ldap.as_ref().ok_or_else(auth_unavailable)?;

        let req = body.into_inner();
        let user = ldap
            .authenticate(&req.username, &req.password)
            .await
            .map_err(session_error_to_http)?;

        let roles = &user.roles;
        let token = jwt
            .create_token(user.uuid, &user.login, roles)
            .map_err(session_error_to_http)?;
        let refresh_token = jwt
            .create_refresh_token(user.uuid, &user.login, roles)
            .await;

        let is_admin = triton_auth_session::roles_imply_admin(roles);
        let user_info = UserInfo {
            id: user.uuid,
            username: user.login.clone(),
            email: user.email.clone(),
            name: user.cn.clone(),
            company: user.company.clone(),
            is_admin,
        };

        let cookie = build_auth_cookie(&token, jwt.access_ttl_secs(), ctx.cookie_secure);
        let mut response = HttpResponseHeaders::new_unnamed(HttpResponseOk(LoginResponse {
            token,
            refresh_token,
            user: user_info,
        }));
        set_cookie_header(response.headers_mut(), cookie);
        Ok(response)
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
        set_cookie_header(response.headers_mut(), cookie);
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
}

fn build_auth_cookie(token: &str, max_age: u64, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!("auth={token}; HttpOnly{secure_flag}; SameSite=Strict; Path=/; Max-Age={max_age}")
}

fn set_cookie_header(headers: &mut http::HeaderMap, cookie: String) {
    match http::HeaderValue::from_str(&cookie) {
        Ok(value) => {
            headers.insert(http::header::SET_COOKIE, value);
        }
        Err(e) => {
            // Cookie content we construct is ASCII-safe; a failure here
            // implies a bug in the token builder, not bad input.
            warn!("failed to build Set-Cookie header: {e}");
        }
    }
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
        SessionError::LdapUnavailable(msg) => {
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "triton_api_server=info,dropshot=info",
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

    let context = ApiContext {
        jwt,
        ldap,
        cookie_secure,
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
