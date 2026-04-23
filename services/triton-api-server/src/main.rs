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
use triton_auth::{auth_scheme, http_sig};
use triton_auth_session::{
    JwtConfig as SessionJwtConfig, JwtService, LdapConfig as SessionLdapConfig, LdapService,
    MahiService, Role, SessionError,
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
    mahi: Option<MahiConfigFile>,
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
        let mahi = ctx.mahi.as_ref().ok_or_else(auth_unavailable)?;

        let req = body.into_inner();
        let user = ldap
            .authenticate(&req.username, &req.password)
            .await
            .map_err(session_error_to_http)?;

        // Password is verified; mahi now provides the canonical operator /
        // group view. A 404 here would mean mahi hasn't caught up with a
        // brand-new user yet; MahiService maps that to AuthenticationFailed
        // so the client can retry.
        let auth_info = mahi
            .lookup(&user.login)
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
    set_cookie_header(response.headers_mut(), cookie);
    Ok(response)
}

/// Issue `(access_token, refresh_token, user_info)` from a verified
/// mahi account record, plus the `Set-Cookie` header for browser
/// clients. Shared tail of `auth_login` (password-verified) and
/// `auth_login_ssh` (signature-verified); everything past the auth
/// primitive is identical.
async fn issue_login_response(
    jwt: &JwtService,
    auth_info: &triton_auth_session::AuthInfo,
    cookie_secure: bool,
) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError> {
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

    let cookie = build_auth_cookie(&token, jwt.access_ttl_secs(), cookie_secure);
    let mut response = HttpResponseHeaders::new_unnamed(HttpResponseOk(LoginResponse {
        token,
        refresh_token,
        user: user_info,
    }));
    set_cookie_header(response.headers_mut(), cookie);
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

/// Install the `ring` rustls crypto provider for this process.
///
/// `reqwest` is built with the workspace-level `rustls-no-provider` feature,
/// so constructing any `reqwest::Client` without a preconfigured TLS config
/// — which is what `mahi-client::Client::new` does under the hood — panics
/// with "No provider set" unless a default `CryptoProvider` has been
/// installed. This mirrors the mitigation already in place in
/// `bugview-service` and `triton-gateway`; a lasting fix would install the
/// provider once for the whole workspace. Idempotent: the second call
/// returns `Err`, which we discard.
fn install_default_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[tokio::main]
async fn main() -> Result<()> {
    install_default_crypto_provider();

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

    let context = ApiContext {
        jwt,
        ldap,
        mahi,
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
