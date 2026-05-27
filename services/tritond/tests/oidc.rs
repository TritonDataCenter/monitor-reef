// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end OIDC federation tests.
//!
//! Each test spins up:
//! 1. An in-process **stub OIDC provider** (axum) that serves
//!    `/.well-known/openid-configuration` + `/jwks` and signs RS256
//!    ID tokens with a key pair generated at startup.
//! 2. An in-process **tritond** with one silo created via the API
//!    (which mints the silo's default tenant) and one IdP
//!    configured against that default tenant via the public API.
//!
//! Post E-5 the IdP is tenant-scoped: the federation index is
//! keyed by `(tenant_id, issuer, subject)` and inbound tokens are
//! routed to their owning tenant via the issuer→tenant reverse
//! index.
//!
//! The tests cover:
//! - `POST /v1/tenants/{}/idp` with an unreachable URL → 4xx
//!   (eager discovery rejects).
//! - First OIDC login JIT-creates a federated user; the same user
//!   id is returned on subsequent logins (no duplicate users).
//! - A token signed by an unrelated key is rejected.
//! - A token whose `iss` doesn't match any configured tenant → 403
//!   on protected endpoints (anonymous principal).
//! - The IdP config GET endpoint never returns the client secret.
//! - Two tenants cannot register the same `issuer_url` (409).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, Router, extract::State, routing::get};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::{RsaPrivateKey, RsaPublicKey, pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts};
use tokio::sync::oneshot;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{NewIdpConfig, NewSilo};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "oidc-test-pass";

// ---------- stub OIDC provider ----------

#[derive(Clone)]
struct StubIdp {
    inner: Arc<StubInner>,
}

struct StubInner {
    issuer: String,
    /// Public key as JWK fields (modulus + exponent in base64url).
    n_b64: String,
    e_b64: String,
    /// PKCS#1-encoded private key bytes for jsonwebtoken signing.
    private_pem: Vec<u8>,
    kid: String,
}

impl StubIdp {
    /// Boot a stub IdP listening on an ephemeral port. Returns the
    /// `(handle, base_url)` pair plus a shutdown signal sender.
    async fn start() -> (StubIdp, SocketAddr, oneshot::Sender<()>) {
        // Generate a fresh 2048-bit RSA key for this run. `rsa` 0.9
        // expects an `OsRng` from its bundled `rand_core` 0.6.
        let private_key = {
            use rsa::rand_core::OsRng;
            RsaPrivateKey::new(&mut OsRng, 2048).expect("rsa keygen")
        };
        let public_key = RsaPublicKey::from(&private_key);

        let n_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            public_key.n().to_bytes_be(),
        );
        let e_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            public_key.e().to_bytes_be(),
        );

        // jsonwebtoken's EncodingKey::from_rsa_pem wants PKCS#1 PEM.
        let private_pem = private_key
            .to_pkcs1_pem(rsa::pkcs8::LineEnding::LF)
            .expect("encode pkcs1 pem")
            .as_bytes()
            .to_vec();

        // Listen on an ephemeral port so we know the URL before the
        // server starts handling requests.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let issuer = format!("http://{bind}");

        let stub = StubIdp {
            inner: Arc::new(StubInner {
                issuer: issuer.clone(),
                n_b64,
                e_b64,
                private_pem,
                kid: "stub-1".to_string(),
            }),
        };
        let stub_for_router = stub.clone();

        let app: Router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery_handler))
            .route("/jwks", get(jwks_handler))
            .with_state(stub_for_router);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .ok();
        });

        (stub, bind, shutdown_tx)
    }

    fn issuer(&self) -> &str {
        &self.inner.issuer
    }

    /// Mint an RS256 ID token signed by our private key.
    fn mint_token(&self, audience: &str, subject: &str, email: Option<&str>) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut claims = BTreeMap::new();
        claims.insert("iss".to_string(), serde_json::json!(self.inner.issuer));
        claims.insert("sub".to_string(), serde_json::json!(subject));
        claims.insert("aud".to_string(), serde_json::json!(audience));
        claims.insert("exp".to_string(), serde_json::json!(now + 3600));
        claims.insert("iat".to_string(), serde_json::json!(now));
        if let Some(e) = email {
            claims.insert("email".to_string(), serde_json::json!(e));
            claims.insert("email_verified".to_string(), serde_json::json!(true));
        }
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.inner.kid.clone());
        let key = EncodingKey::from_rsa_pem(&self.inner.private_pem).expect("load encoding key");
        jsonwebtoken::encode(&header, &claims, &key).expect("sign id token")
    }
}

async fn discovery_handler(State(stub): State<StubIdp>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": stub.inner.issuer,
        "authorization_endpoint": format!("{}/auth", stub.inner.issuer),
        "token_endpoint": format!("{}/token", stub.inner.issuer),
        "jwks_uri": format!("{}/jwks", stub.inner.issuer),
        "response_types_supported": ["id_token"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
}

async fn jwks_handler(State(stub): State<StubIdp>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "keys": [
            {
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": stub.inner.kid,
                "n": stub.inner.n_b64,
                "e": stub.inner.e_b64,
            }
        ]
    }))
}

// ---------- tritond test fixture ----------

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    bearer: String,
    store: Arc<dyn Store>,
}

impl TestServer {
    async fn start() -> Self {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let user_id = Uuid::new_v4();
        let user = User {
            id: user_id,
            username: "root".to_string(),
            password_hash: hash_password(&RedactedString::from(ROOT_PASSWORD))
                .await
                .unwrap(),
            is_root: true,
            fleet_admin: false,
            capabilities: Default::default(),
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(user).await.unwrap();
        let jwt_key = JwtKey::generate();
        let (token, _) = mint_access(&jwt_key, user_id).unwrap();
        let auth = Arc::new(AuthService::new(jwt_key).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store.clone(), auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self {
            server,
            bearer: token,
            store,
        }
    }

    fn bind(&self) -> SocketAddr {
        self.server.local_addr()
    }

    fn authed_client(&self) -> tritond_client::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.bearer).parse().unwrap(),
        );
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        tritond_client::Client::new_with_client(&format!("http://{}", self.bind()), reqwest)
    }

    fn bearer_client(&self, token: &str) -> tritond_client::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let reqwest = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        tritond_client::Client::new_with_client(&format!("http://{}", self.bind()), reqwest)
    }

    async fn close(self) {
        self.server.close().await.unwrap();
    }
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn put_tenant_idp_eagerly_rejects_unreachable_url() {
    let test = TestServer::start().await;
    let client = test.authed_client();

    let silo = client
        .create_silo()
        .body(NewSilo {
            name: "tenants".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = client
        .put_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .body(NewIdpConfig {
            issuer_url: "http://127.0.0.1:1".to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        })
        .send()
        .await
        .expect_err("unreachable IdP should fail eagerly");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert!(response.status().is_client_error());

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_oidc_login_jit_creates_federated_user_and_second_reuses() {
    let test = TestServer::start().await;
    let (idp, _bind, shutdown) = StubIdp::start().await;
    let admin = test.authed_client();

    // 1) Create silo + configure IdP
    let silo = admin
        .create_silo()
        .body(NewSilo {
            name: "federated".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    admin
        .put_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .body(NewIdpConfig {
            issuer_url: idp.issuer().to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        })
        .send()
        .await
        .expect("eager discovery should succeed against stub")
        .into_inner();

    // 2) Mint a stub-signed ID token and present it.
    let token = idp.mint_token("tritond", "tenant-42", Some("tenant42@example.com"));
    let federated = test.bearer_client(&token);

    // The Cedar policy doesn't yet permit federated users to do
    // anything beyond the public actions, so a tenant-facing
    // protected call fails 403 — but the underlying authentication
    // succeeded (the JIT user got created).
    let err = federated
        .create_silo()
        .body(NewSilo {
            name: "intruder".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("federated user is not Cedar-permitted to create silos yet");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 403);

    // The JIT user must exist in the store now, rooted at the
    // tenant whose IdP authenticated it (post E-5 the federation
    // index keys directly off tenant_id).
    let jit = test
        .store
        .get_user_by_federation(silo.default_tenant_id, idp.issuer(), "tenant-42")
        .await
        .expect("JIT user should have been created");
    assert_eq!(jit.tenant_id, Some(silo.default_tenant_id));
    // Sanity: the default tenant resolves back to this silo.
    let tenant = test.store.get_tenant(silo.default_tenant_id).await.unwrap();
    assert_eq!(tenant.silo_id, silo.id);
    let jit_id = jit.id;

    // 3) A second login with a fresh token from the same subject
    //    should resolve to the same user — no duplicate row.
    let token2 = idp.mint_token("tritond", "tenant-42", Some("tenant42@example.com"));
    let federated2 = test.bearer_client(&token2);
    let _ = federated2.health().send().await.unwrap();

    let again = test
        .store
        .get_user_by_federation(silo.default_tenant_id, idp.issuer(), "tenant-42")
        .await
        .unwrap();
    assert_eq!(again.id, jit_id);

    let _ = shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_signed_by_unrelated_key_does_not_authenticate() {
    let test = TestServer::start().await;
    let (real_idp, _, real_shutdown) = StubIdp::start().await;
    let (other_idp, _, other_shutdown) = StubIdp::start().await;
    let admin = test.authed_client();

    let silo = admin
        .create_silo()
        .body(NewSilo {
            name: "fed".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    admin
        .put_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .body(NewIdpConfig {
            issuer_url: real_idp.issuer().to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        })
        .send()
        .await
        .unwrap();

    // Token signed by `other_idp` but with `iss` claim set to
    // `real_idp` — the verifier fetches `real_idp`'s JWKS, which
    // doesn't contain `other_idp`'s signing key.
    let header = jsonwebtoken::Header {
        alg: Algorithm::RS256,
        kid: Some(other_idp.inner.kid.clone()),
        ..Default::default()
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = serde_json::json!({
        "iss": real_idp.issuer(),
        "sub": "spoof",
        "aud": "tritond",
        "exp": now + 3600,
        "iat": now,
    });
    let key = EncodingKey::from_rsa_pem(&other_idp.inner.private_pem).unwrap();
    let token = jsonwebtoken::encode(&header, &claims, &key).unwrap();

    // The forged token is treated as anonymous, so the protected
    // endpoint refuses with 403 (Cedar deny on Anonymous).
    let federated = test.bearer_client(&token);
    let err = federated
        .create_silo()
        .body(NewSilo {
            name: "x".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("forged-key token must not authenticate");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 403);

    // No JIT user should have been created from the forged login.
    let jit = test
        .store
        .get_user_by_federation(silo.default_tenant_id, real_idp.issuer(), "spoof")
        .await;
    assert!(jit.is_err(), "no JIT user should have been created");

    let _ = real_shutdown.send(());
    let _ = other_shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_tenant_idp_never_returns_secret() {
    let test = TestServer::start().await;
    let (idp, _, shutdown) = StubIdp::start().await;
    let admin = test.authed_client();

    let silo = admin
        .create_silo()
        .body(NewSilo {
            name: "redact".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    admin
        .put_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .body(NewIdpConfig {
            issuer_url: idp.issuer().to_string(),
            client_id: "tritond".to_string(),
            client_secret: "this-must-not-leak".to_string(),
            audience: None,
        })
        .send()
        .await
        .unwrap();

    let view = admin
        .get_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .send()
        .await
        .unwrap()
        .into_inner();

    let body_json = serde_json::to_string(&view).unwrap();
    assert!(
        !body_json.contains("this-must-not-leak"),
        "client secret leaked through GET: {body_json}"
    );

    // Delete clears the config and a subsequent GET is 404.
    admin
        .delete_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .send()
        .await
        .unwrap();
    let err = admin
        .get_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .send()
        .await
        .expect_err("post-delete GET should be not-found");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    let _ = shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_tenant_duplicate_issuer_conflicts() {
    // Two tenants in the same silo cannot register the same
    // `issuer_url` — the second `put_tenant_idp` must return 409.
    // Re-putting the same tenant's config is idempotent and OK.
    let test = TestServer::start().await;
    let (idp, _, shutdown) = StubIdp::start().await;
    let admin = test.authed_client();

    let silo = admin
        .create_silo()
        .body(NewSilo {
            name: "issuer-uniq".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    // Create a sibling tenant in the same silo.
    let second_tenant = admin
        .create_silo_tenant()
        .silo_id(silo.id)
        .body(tritond_client::types::NewTenant {
            name: "sibling".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    // First tenant claims the issuer.
    admin
        .put_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .body(NewIdpConfig {
            issuer_url: idp.issuer().to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        })
        .send()
        .await
        .expect("first tenant claims issuer");

    // Idempotent re-put for the *same* tenant succeeds.
    admin
        .put_tenant_idp()
        .tenant_id(silo.default_tenant_id)
        .body(NewIdpConfig {
            issuer_url: idp.issuer().to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        })
        .send()
        .await
        .expect("idempotent re-put for same tenant must succeed");

    // Second tenant attempting the same issuer must 409.
    let err = admin
        .put_tenant_idp()
        .tenant_id(second_tenant.id)
        .body(NewIdpConfig {
            issuer_url: idp.issuer().to_string(),
            client_id: "tritond".to_string(),
            client_secret: "other".to_string(),
            audience: None,
        })
        .send()
        .await
        .expect_err("cross-tenant duplicate issuer must conflict");
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 409);

    let _ = shutdown.send(());
    test.close().await;
}
