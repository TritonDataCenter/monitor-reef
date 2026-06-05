// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Authentication support for triton-gateway client requests.
//!
//! The gateway accepts two wire-level auth styles:
//!
//! 1. **Bearer JWT** (primary path). The CLI logs in via
//!    `POST /v1/auth/login`, holds on to the access + refresh token pair, and
//!    stamps `Authorization: Bearer <jwt>` on every outgoing request. Token
//!    storage and refresh policy live behind the [`TokenProvider`] trait so
//!    this crate can stay agnostic about where tokens come from.
//! 2. **SSH HTTP Signature** (fallback / dev-test path). Identical to the
//!    scheme `cloudapi-client` uses today; delegates to
//!    [`triton_auth::sign_request`]. Useful when pointing the gateway-client
//!    at a gateway build that accepts both auth styles, or when driving
//!    cloudapi-compatible paths directly.
//!
//! The active style is selected per-client by the [`GatewayAuthMethod`] the
//! caller passes when constructing a client.

use std::sync::Arc;

use triton_auth::AuthConfig;

/// Error type returned by the `pre_hook_async` on the generated client.
///
/// `reqwest::Client` hook signatures expect `Box<dyn Error + Send + Sync>`.
type HookError = Box<dyn std::error::Error + Send + Sync>;

/// How the gateway client authenticates outgoing requests.
///
/// Bearer is the primary path for operators using a tritonapi profile; SshKey
/// is kept for dev/test parity with the existing cloudapi-client flow.
#[derive(Clone)]
pub enum GatewayAuthMethod {
    /// Stamp `Authorization: Bearer <jwt>` using a pluggable token source.
    ///
    /// `account` is the account name to substitute into `/{account}/*`
    /// path segments on cloudapi-proxied calls. The JWT itself identifies
    /// the caller but Progenitor's generated builders need the account as
    /// a plain string argument -- for single-user profiles it's just the
    /// profile's `account` field; for sub-user flows it's still the owning
    /// account (the sub-user's resources live under it).
    Bearer {
        provider: Arc<dyn TokenProvider>,
        account: String,
    },
    /// Sign the request with an SSH key (HTTP Signature / RFC 6789 style).
    SshKey(AuthConfig),
}

impl std::fmt::Debug for GatewayAuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer { account, .. } => f
                .debug_struct("Bearer")
                .field("provider", &"<TokenProvider>")
                .field("account", account)
                .finish(),
            Self::SshKey(cfg) => f.debug_tuple("SshKey").field(cfg).finish(),
        }
    }
}

/// Abstracts token access + refresh policy for the Bearer auth path.
///
/// Implementations are responsible for:
/// - Returning the currently-valid access token (optionally refreshing
///   proactively when near expiry) from [`current_token`](Self::current_token).
/// - Reacting to a 401 from the gateway by attempting a refresh in
///   [`on_unauthorized`](Self::on_unauthorized), returning `Ok(())` to signal
///   that the next request should be retried or `Err` if re-authentication
///   is required.
///
/// # Who calls `on_unauthorized`?
///
/// **Not this crate.** In the current (Phase 2) shape the gateway-client
/// stamps headers via [`add_auth_headers`] and otherwise lets Progenitor's
/// generated code send the request verbatim. There is no automatic
/// retry-on-401 loop inside the client itself.
///
/// The 401-retry dance is owned by the layer that wires the TokenProvider
/// up to a CLI (Phase 3/4). The trait exists in this crate now to stabilize
/// the interface so the Phase 3 `FileTokenProvider` and the Phase 4 request
/// adapter can both target it without a breaking change later.
#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    /// Return the current access token. Implementations may refresh
    /// proactively if the token is near expiry; they MUST NOT block on
    /// interactive prompts — those belong in a `triton login` command.
    async fn current_token(&self) -> anyhow::Result<String>;

    /// Called after the caller observes a 401 from the gateway.
    /// Implementations should attempt a refresh and return Ok(()) if the
    /// next request should be retried, or Err if re-authentication is
    /// required.
    ///
    /// Note: this crate does not invoke this method today; see the trait
    /// docs above for the phase-split rationale.
    async fn on_unauthorized(&self) -> anyhow::Result<()>;
}

/// Per-request config the gateway client's pre-hook sees.
///
/// Small wrapper around [`GatewayAuthMethod`] plus any orthogonal headers
/// the gateway (or cloudapi-proxied paths) may need:
///
/// * `accept_version` -- value for `Accept-Version`, currently used by the
///   cloudapi-proxied surface for API versioning.
/// * `act_as` -- value for `X-Act-As`, for operator masquerading against
///   cloudapi-proxied paths. Meaningless for `/v1/auth/*`.
///
/// These are set to `None` by default and may be ignored by the gateway for
/// tritonapi-native endpoints; exposing them here keeps the shape parallel
/// with `cloudapi-client`'s `AuthConfig` so Phase 4 CLI wiring can forward
/// the same precedence rules without adding fields later.
#[derive(Clone, Debug)]
pub struct GatewayAuthConfig {
    /// Selected wire-level authentication scheme.
    pub method: GatewayAuthMethod,
    /// Optional `Accept-Version` header value.
    pub accept_version: Option<String>,
    /// Optional `X-Act-As` header value (operator masquerade).
    pub act_as: Option<String>,
}

impl GatewayAuthConfig {
    /// Construct a Bearer-auth config with no orthogonal headers set.
    ///
    /// `account` is the account name that gets stamped into
    /// `/{account}/*` cloudapi-proxied paths (see [`GatewayAuthMethod::Bearer`]
    /// for the sub-user caveat).
    pub fn bearer(provider: Arc<dyn TokenProvider>, account: impl Into<String>) -> Self {
        Self {
            method: GatewayAuthMethod::Bearer {
                provider,
                account: account.into(),
            },
            accept_version: None,
            act_as: None,
        }
    }

    /// Construct an SSH-key auth config with no orthogonal headers set.
    ///
    /// Note: `AuthConfig` already carries `accept_version` and `act_as` for
    /// the SSH path; we intentionally leave the `GatewayAuthConfig` wrappers
    /// as `None` in that case to keep exactly one source of truth. The SSH
    /// branch of [`add_auth_headers`] pulls these from the inner
    /// `AuthConfig`.
    pub fn ssh_key(cfg: AuthConfig) -> Self {
        Self {
            method: GatewayAuthMethod::SshKey(cfg),
            accept_version: None,
            act_as: None,
        }
    }

    /// Set `Accept-Version` (cloudapi-proxied endpoints).
    pub fn with_accept_version(mut self, v: impl Into<String>) -> Self {
        self.accept_version = Some(v.into());
        self
    }

    /// Set `X-Act-As` (operator masquerade on cloudapi-proxied endpoints).
    pub fn with_act_as(mut self, v: impl Into<String>) -> Self {
        self.act_as = Some(v.into());
        self
    }
}

/// Add authentication headers to an outgoing request.
///
/// Used as the `pre_hook_async` for the Progenitor-generated client. Branches
/// on [`GatewayAuthConfig::method`] to stamp the appropriate Authorization
/// scheme and (for the SSH path) a `Date` header.
///
/// # Errors
/// Returns an error if
/// - Bearer: the [`TokenProvider`] fails to produce a token, or the returned
///   token is not a valid HTTP header value.
/// - SshKey: signing fails, or the header values can't be constructed.
pub async fn add_auth_headers(
    cfg: &GatewayAuthConfig,
    request: &mut reqwest::Request,
) -> Result<(), HookError> {
    match &cfg.method {
        GatewayAuthMethod::Bearer { provider, .. } => {
            stamp_bearer(provider.as_ref(), request).await?;
        }
        GatewayAuthMethod::SshKey(auth_cfg) => {
            stamp_ssh(auth_cfg, request).await?;
        }
    }

    // Orthogonal headers (top-level config wins over the inner AuthConfig so
    // callers can override per-request without rebuilding the AuthConfig).
    // For SshKey, the `stamp_ssh` helper above has already forwarded the
    // inner AuthConfig's accept_version / act_as; re-applying here is
    // idempotent when values match and an intentional override otherwise.
    if let Some(v) = &cfg.accept_version {
        insert_header(request, "accept-version", v)?;
    }
    if let Some(v) = &cfg.act_as {
        insert_header(request, "x-act-as", v)?;
    }

    Ok(())
}

/// Stamp `Authorization: Bearer <jwt>`.
///
/// Bearer auth needs no Date header, no query-string mutation, and no
/// request-signing machinery — the JWT carries its own expiry, and transport
/// integrity is TLS's problem.
async fn stamp_bearer(
    provider: &dyn TokenProvider,
    request: &mut reqwest::Request,
) -> Result<(), HookError> {
    let token = provider
        .current_token()
        .await
        .map_err(|e| -> HookError { e.into() })?;

    let value = format!("Bearer {token}");
    let hv = value.parse::<reqwest::header::HeaderValue>().map_err(|e| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid bearer header: {e}"),
        )) as HookError
    })?;
    request
        .headers_mut()
        .insert(reqwest::header::AUTHORIZATION, hv);
    Ok(())
}

/// Stamp Date + Authorization for the SSH HTTP Signature fallback path.
///
/// This mirrors `cloudapi-client`'s `add_auth_headers` (same crate, different
/// callsite) and intentionally duplicates the shape rather than calling into
/// `cloudapi-client`. Sharing this would require extracting a helper into
/// `triton-auth` (or a new shared crate) and is out of scope for Phase 2
/// since the plan explicitly forbids touching cloudapi-client. The duplication
/// is ~30 lines.
async fn stamp_ssh(
    auth_config: &AuthConfig,
    request: &mut reqwest::Request,
) -> Result<(), HookError> {
    let method = request.method().as_str().to_string();

    // Re-read the path+query (no as-role injection here; the gateway does not
    // carry RBAC role parameters — those are cloudapi-only and are added by
    // cloudapi-client for its direct-to-cloudapi case).
    let url = request.url();
    let path_and_query = match url.query() {
        Some(q) => format!("{}?{}", url.path(), q),
        None => url.path().to_string(),
    };

    let (date_header, auth_header) =
        triton_auth::sign_request(auth_config, &method, &path_and_query)
            .await
            .map_err(|e| -> HookError { Box::new(e) })?;

    let headers = request.headers_mut();
    headers.insert(
        reqwest::header::DATE,
        date_header.parse().map_err(|e| {
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid date header: {e}"),
            )) as HookError
        })?,
    );
    headers.insert(
        reqwest::header::AUTHORIZATION,
        auth_header.parse().map_err(|e| {
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid authorization header: {e}"),
            )) as HookError
        })?,
    );

    // Forward the inner AuthConfig's orthogonal headers for the SSH path.
    // (On the Bearer path these must be set via GatewayAuthConfig directly.)
    if let Some(act_as) = &auth_config.act_as {
        insert_header(request, "x-act-as", act_as)?;
    }
    if let Some(version) = &auth_config.accept_version {
        insert_header(request, "accept-version", version)?;
    }

    Ok(())
}

/// Insert a header by lowercase name + string value, mapping parse errors
/// into the hook's boxed-error return type.
fn insert_header(
    request: &mut reqwest::Request,
    name: &'static str,
    value: &str,
) -> Result<(), HookError> {
    let hn = reqwest::header::HeaderName::from_static(name);
    let hv = value.parse::<reqwest::header::HeaderValue>().map_err(|e| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid {name} header: {e}"),
        )) as HookError
    })?;
    request.headers_mut().insert(hn, hv);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use triton_auth::KeySource;

    /// Test TokenProvider that returns a canned token and records calls to
    /// `on_unauthorized` so tests can assert refresh-hook invocation.
    struct MockTokenProvider {
        token: String,
        unauth_calls: AtomicUsize,
    }

    impl MockTokenProvider {
        fn new(token: impl Into<String>) -> Self {
            Self {
                token: token.into(),
                unauth_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl TokenProvider for MockTokenProvider {
        async fn current_token(&self) -> anyhow::Result<String> {
            Ok(self.token.clone())
        }

        async fn on_unauthorized(&self) -> anyhow::Result<()> {
            self.unauth_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn blank_request() -> reqwest::Request {
        let url = "https://gateway.example.com/v1/ping"
            .parse::<reqwest::Url>()
            .expect("static url parses");
        reqwest::Request::new(reqwest::Method::GET, url)
    }

    #[tokio::test]
    async fn bearer_stamps_authorization_header() {
        let provider = Arc::new(MockTokenProvider::new("TEST-JWT-TOKEN"));
        let cfg = GatewayAuthConfig::bearer(provider.clone(), "test-account");

        let mut req = blank_request();
        add_auth_headers(&cfg, &mut req)
            .await
            .expect("stamping succeeds");

        let auth = req
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("authorization present");
        assert_eq!(auth.to_str().unwrap(), "Bearer TEST-JWT-TOKEN");

        // Bearer path must not stamp a Date header (JWT carries its own exp).
        assert!(
            req.headers().get(reqwest::header::DATE).is_none(),
            "bearer path should not set Date"
        );

        // Nor should it emit a Signature-scheme Authorization.
        assert!(
            !auth.to_str().unwrap().starts_with("Signature "),
            "bearer path must not produce HTTP Signature header"
        );
    }

    #[tokio::test]
    async fn bearer_stamps_orthogonal_headers_when_set() {
        let provider = Arc::new(MockTokenProvider::new("T"));
        let cfg = GatewayAuthConfig::bearer(provider, "test-account")
            .with_accept_version("~9")
            .with_act_as("other-account");

        let mut req = blank_request();
        add_auth_headers(&cfg, &mut req)
            .await
            .expect("stamping succeeds");

        assert_eq!(
            req.headers()
                .get("accept-version")
                .unwrap()
                .to_str()
                .unwrap(),
            "~9"
        );
        assert_eq!(
            req.headers().get("x-act-as").unwrap().to_str().unwrap(),
            "other-account"
        );
    }

    /// Asserts the TokenProvider trait is dyn-compatible (`Arc<dyn ...>`)
    /// and that `on_unauthorized` has the expected signature callable through
    /// the trait object. Phase 2 does not invoke `on_unauthorized` from the
    /// client itself; this test exists to lock the interface now.
    #[tokio::test]
    async fn token_provider_is_dyn_compatible_and_on_unauthorized_is_callable() {
        let provider: Arc<dyn TokenProvider> = Arc::new(MockTokenProvider::new("x"));
        // Must be Send + Sync so it can cross await points.
        fn assert_send_sync<T: Send + Sync>(_: &T) {}
        assert_send_sync(&provider);

        // The hook is stable callable and returns Result<()>.
        provider.on_unauthorized().await.expect("hook is invokable");
    }

    /// SSH HTTP Signature path: uses a checked-in RSA test key from
    /// `libs/triton-auth/tests/keys/` and asserts both Date and an
    /// Authorization header of the `Signature ...` scheme are stamped.
    #[tokio::test]
    async fn ssh_key_stamps_signature_and_date_headers() {
        // Path resolved at test time via CARGO_MANIFEST_DIR so the test is
        // portable across checkouts.
        let key_path = format!(
            "{}/../../../libs/triton-auth/tests/keys/id_rsa",
            env!("CARGO_MANIFEST_DIR")
        );
        const ID_RSA_MD5: &str = "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6";

        let auth =
            AuthConfig::new("testacct", KeySource::file(&key_path)).with_accept_version("~9");
        // Sanity: the configured fingerprint is the one triton-auth's own
        // tests pin for this key; we include it here just to document which
        // key-id string we expect to appear in the signed header.
        let _ = ID_RSA_MD5;

        let cfg = GatewayAuthConfig::ssh_key(auth);

        let mut req = blank_request();
        add_auth_headers(&cfg, &mut req)
            .await
            .expect("SSH signing succeeds with the test fixture key");

        let date = req
            .headers()
            .get(reqwest::header::DATE)
            .expect("Date present on SSH path");
        // Format is RFC 2822; just sanity-check it ends in " GMT".
        assert!(
            date.to_str().unwrap().ends_with("GMT"),
            "Date header should be RFC 2822 GMT: got {date:?}"
        );

        let auth_val = req
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("Authorization present on SSH path");
        let auth_str = auth_val.to_str().unwrap();
        assert!(
            auth_str.starts_with("Signature "),
            "SSH path should produce HTTP Signature scheme: got {auth_str}"
        );
        // Must carry keyId, algorithm, signature components.
        assert!(auth_str.contains("keyId="), "missing keyId in {auth_str}");
        assert!(
            auth_str.contains("algorithm="),
            "missing algorithm in {auth_str}"
        );
        assert!(
            auth_str.contains("signature="),
            "missing signature in {auth_str}"
        );
        // Accept-Version forwarded from inner AuthConfig on SSH path.
        assert_eq!(
            req.headers()
                .get("accept-version")
                .unwrap()
                .to_str()
                .unwrap(),
            "~9"
        );
    }
}
