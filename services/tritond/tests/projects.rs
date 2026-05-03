// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the `/v2/silos/{silo_id}/projects` surface
//! and the silo-scoped Cedar policy that gates it.
//!
//! Each test spins up a stub OIDC provider so we can mint federated
//! ID tokens for tenant principals and exercise the cross-silo 404
//! invariant directly.

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
use tritond_client::types::{NewIdpConfig, NewProject, NewSilo};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "projects-test";

// ---------- stub OIDC provider (same shape as oidc.rs) ----------

#[derive(Clone)]
struct StubIdp {
    inner: Arc<StubInner>,
}

struct StubInner {
    issuer: String,
    n_b64: String,
    e_b64: String,
    private_pem: Vec<u8>,
    kid: String,
}

impl StubIdp {
    async fn start() -> (StubIdp, SocketAddr, oneshot::Sender<()>) {
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
        let private_pem = private_key
            .to_pkcs1_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap()
            .as_bytes()
            .to_vec();
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
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
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

    fn mint_token(&self, audience: &str, subject: &str) -> String {
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
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.inner.kid.clone());
        let key = EncodingKey::from_rsa_pem(&self.inner.private_pem).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }
}

async fn discovery(State(stub): State<StubIdp>) -> Json<serde_json::Value> {
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

async fn jwks(State(stub): State<StubIdp>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": stub.inner.kid,
            "n": stub.inner.n_b64,
            "e": stub.inner.e_b64,
        }]
    }))
}

// ---------- test fixture ----------

struct TestServer {
    server: dropshot::HttpServer<ApiContext>,
    root_bearer: String,
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
            created_at: Utc::now(),
            tenant_id: None,
            federation: None,
        };
        store.create_user(user).await.unwrap();
        let jwt_key = JwtKey::generate();
        let (token, _) = mint_access(&jwt_key, user_id).unwrap();
        let auth = Arc::new(AuthService::new(jwt_key).unwrap());
        let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
        let context = ApiContext::new(store, auth, audit);
        let server = start_server_with_context("127.0.0.1:0", context)
            .await
            .unwrap();
        Self {
            server,
            root_bearer: token,
        }
    }

    fn bind(&self) -> SocketAddr {
        self.server.local_addr()
    }

    fn root_client(&self) -> tritond_client::Client {
        self.bearer_client(&self.root_bearer)
    }

    fn anonymous_client(&self) -> tritond_client::Client {
        tritond_client::Client::new(&format!("http://{}", self.bind()))
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

fn assert_status(err: progenitor_client::Error<tritond_client::types::Error>, want: u16) {
    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), want);
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn root_can_create_and_read_projects_in_any_silo() {
    let test = TestServer::start().await;
    let root = test.root_client();

    let silo_a = root
        .create_silo()
        .body(NewSilo {
            name: "alpha".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let silo_b = root
        .create_silo()
        .body(NewSilo {
            name: "beta".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let p_a = root
        .create_silo_project()
        .silo_id(silo_a.id)
        .body(NewProject {
            name: "p1".to_string(),
            description: Some("alpha-side".to_string()),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let p_b = root
        .create_silo_project()
        .silo_id(silo_b.id)
        .body(NewProject {
            name: "p1".to_string(), // same name in different silo: must succeed
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_ne!(p_a.id, p_b.id);

    let listed_a = root
        .list_silo_projects()
        .silo_id(silo_a.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed_a.len(), 1);
    assert_eq!(listed_a[0].id, p_a.id);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn federated_user_can_act_in_own_silo_only() {
    let test = TestServer::start().await;
    let (idp, _, shutdown) = StubIdp::start().await;
    let root = test.root_client();

    let silo_alpha = root
        .create_silo()
        .body(NewSilo {
            name: "alpha".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let silo_beta = root
        .create_silo()
        .body(NewSilo {
            name: "beta".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    // Configure IdP only for silo alpha; tokens from this IdP get
    // mapped to alpha-membership.
    root.put_silo_idp()
        .silo_id(silo_alpha.id)
        .body(NewIdpConfig {
            issuer_url: idp.issuer().to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        })
        .send()
        .await
        .unwrap();

    let token = idp.mint_token("tritond", "tenant-42");
    let tenant = test.bearer_client(&token);

    // Tenant in alpha → can create a project in alpha.
    let proj = tenant
        .create_silo_project()
        .silo_id(silo_alpha.id)
        .body(NewProject {
            name: "tenant-project".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("alpha-tenant should create a project in alpha")
        .into_inner();
    assert_eq!(proj.silo_id, silo_alpha.id);

    // Same tenant → cannot create a project in beta. Must be 404
    // (cross-silo probe gets the same response as unknown silo).
    let err = tenant
        .create_silo_project()
        .silo_id(silo_beta.id)
        .body(NewProject {
            name: "intruder".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("alpha-tenant must not write into beta");
    assert_status(err, 404);

    // Tenant lists projects in alpha → sees the one they created.
    let listed = tenant
        .list_silo_projects()
        .silo_id(silo_alpha.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, proj.id);

    // Tenant lists projects in beta → 404.
    let err = tenant
        .list_silo_projects()
        .silo_id(silo_beta.id)
        .send()
        .await
        .expect_err("alpha-tenant must not enumerate beta");
    assert_status(err, 404);

    let _ = shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_silo_get_returns_404_not_403() {
    // Even when the project exists, accessing it via the *wrong*
    // silo's path returns 404 — so a tenant in silo A can't probe
    // for silo B's project ids by trial.
    let test = TestServer::start().await;
    let (idp, _, shutdown) = StubIdp::start().await;
    let root = test.root_client();

    let silo_alpha = root
        .create_silo()
        .body(NewSilo {
            name: "alpha".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let silo_beta = root
        .create_silo()
        .body(NewSilo {
            name: "beta".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let beta_project = root
        .create_silo_project()
        .silo_id(silo_beta.id)
        .body(NewProject {
            name: "secret".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    root.put_silo_idp()
        .silo_id(silo_alpha.id)
        .body(NewIdpConfig {
            issuer_url: idp.issuer().to_string(),
            client_id: "tritond".to_string(),
            client_secret: "shhh".to_string(),
            audience: None,
        })
        .send()
        .await
        .unwrap();
    let token = idp.mint_token("tritond", "alpha-tenant");
    let tenant = test.bearer_client(&token);

    // GET /v2/silos/{alpha}/projects/{beta-project-id} — Cedar denies
    // because alpha-tenant is not in beta. The handler returns 404
    // even though beta_project.id is real and would resolve.
    let err = tenant
        .get_silo_project()
        .silo_id(silo_alpha.id)
        .project_id(beta_project.id)
        .send()
        .await
        .expect_err("cross-silo get must 404");
    assert_status(err, 404);

    let _ = shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_cannot_reach_project_endpoints() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let silo = root
        .create_silo()
        .body(NewSilo {
            name: "x".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let anon = test.anonymous_client();
    let err = anon
        .list_silo_projects()
        .silo_id(silo.id)
        .send()
        .await
        .expect_err("anonymous list must be denied");
    assert_status(err, 404);

    test.close().await;
}
