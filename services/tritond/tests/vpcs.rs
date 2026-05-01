// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v2/silos/{silo_id}/projects/{project_id}/vpcs` surface and the
//! silo-scoped Cedar policy that gates it.
//!
//! Mirrors `tests/projects.rs`: the same stub-OIDC fixture mints
//! federated tenant tokens, and the same TestServer pattern spins
//! tritond up in-process. Adds two invariants beyond the projects
//! suite — the VPC must carry an IP plan (at least one of
//! `ipv4_block` / `ipv6_block`) and a cross-*project* probe (same
//! silo, different project) gets the same 404 the cross-silo probe
//! does.

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
use tritond_client::types::{NewIdpConfig, NewProject, NewSilo, NewVpc};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "vpcs-test";

// ---------- stub OIDC provider (same shape as tests/projects.rs) ----------

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
            silo_id: None,
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

fn dual_stack(name: &str) -> NewVpc {
    NewVpc {
        name: name.to_string(),
        description: None,
        ipv4_block: Some("10.0.0.0/24".to_string()),
        ipv6_block: Some("fd00::/48".to_string()),
    }
}

fn ipv4_only(name: &str, cidr: &str) -> NewVpc {
    NewVpc {
        name: name.to_string(),
        description: None,
        ipv4_block: Some(cidr.to_string()),
        ipv6_block: None,
    }
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn root_can_create_and_read_vpcs_in_any_silo_project() {
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
    let proj_a = root
        .create_silo_project()
        .silo_id(silo_a.id)
        .body(NewProject {
            name: "p1".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let proj_b = root
        .create_silo_project()
        .silo_id(silo_b.id)
        .body(NewProject {
            name: "p1".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let v_a = root
        .create_project_vpc()
        .silo_id(silo_a.id)
        .project_id(proj_a.id)
        .body(dual_stack("prod"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let v_b = root
        .create_project_vpc()
        .silo_id(silo_b.id)
        .project_id(proj_b.id)
        .body(dual_stack("prod")) // same name in different silo+project: ok
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_ne!(v_a.id, v_b.id);
    assert_ne!(v_a.vni, v_b.vni);
    assert_eq!(v_a.ipv4_block.as_deref(), Some("10.0.0.0/24"));
    assert_eq!(v_a.ipv6_block.as_deref(), Some("fd00::/48"));

    let listed = root
        .list_project_vpcs()
        .silo_id(silo_a.id)
        .project_id(proj_a.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, v_a.id);

    let fetched = root
        .get_project_vpc()
        .silo_id(silo_a.id)
        .project_id(proj_a.id)
        .vpc_id(v_a.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.vni, v_a.vni);

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
    let alpha_proj = root
        .create_silo_project()
        .silo_id(silo_alpha.id)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let beta_proj = root
        .create_silo_project()
        .silo_id(silo_beta.id)
        .body(NewProject {
            name: "p".to_string(),
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

    let token = idp.mint_token("tritond", "tenant-42");
    let tenant = test.bearer_client(&token);

    // Alpha tenant → can create a VPC in alpha's project.
    let vpc = tenant
        .create_project_vpc()
        .silo_id(silo_alpha.id)
        .project_id(alpha_proj.id)
        .body(dual_stack("tenant-vpc"))
        .send()
        .await
        .expect("alpha-tenant should create a vpc in alpha")
        .into_inner();
    assert_eq!(vpc.silo_id, silo_alpha.id);
    assert_eq!(vpc.project_id, alpha_proj.id);

    // Same tenant → cannot create a VPC in beta's project. 404.
    let err = tenant
        .create_project_vpc()
        .silo_id(silo_beta.id)
        .project_id(beta_proj.id)
        .body(dual_stack("intruder"))
        .send()
        .await
        .expect_err("alpha-tenant must not write into beta");
    assert_status(err, 404);

    // Tenant lists in alpha → sees their VPC.
    let listed = tenant
        .list_project_vpcs()
        .silo_id(silo_alpha.id)
        .project_id(alpha_proj.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, vpc.id);

    // Tenant lists in beta → 404 (cross-silo).
    let err = tenant
        .list_project_vpcs()
        .silo_id(silo_beta.id)
        .project_id(beta_proj.id)
        .send()
        .await
        .expect_err("alpha-tenant must not enumerate beta vpcs");
    assert_status(err, 404);

    let _ = shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_silo_get_returns_404_not_403() {
    // VPC exists in beta. Alpha tenant tries to GET it via alpha's
    // path. Cross-silo deny (Cedar). 404, never 403.
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
    let beta_proj = root
        .create_silo_project()
        .silo_id(silo_beta.id)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let beta_vpc = root
        .create_project_vpc()
        .silo_id(silo_beta.id)
        .project_id(beta_proj.id)
        .body(dual_stack("secret"))
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

    // GET /v2/silos/{alpha}/projects/{beta_proj.id}/vpcs/{beta_vpc.id}
    // — Cedar denies because alpha-tenant is not in beta. 404.
    let err = tenant
        .get_project_vpc()
        .silo_id(silo_alpha.id)
        .project_id(beta_proj.id)
        .vpc_id(beta_vpc.id)
        .send()
        .await
        .expect_err("cross-silo get must 404");
    assert_status(err, 404);

    let _ = shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_project_get_returns_404() {
    // Same silo, different project. Even root must get a 404 if the
    // path's project doesn't actually own the VPC — defence-in-depth
    // on the handler-side project_id check.
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
    let proj_a = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "a".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let proj_b = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "b".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let vpc_in_a = root
        .create_project_vpc()
        .silo_id(silo.id)
        .project_id(proj_a.id)
        .body(dual_stack("net"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .get_project_vpc()
        .silo_id(silo.id)
        .project_id(proj_b.id) // wrong project for this vpc
        .vpc_id(vpc_in_a.id)
        .send()
        .await
        .expect_err("cross-project get must 404");
    assert_status(err, 404);

    let err = root
        .delete_project_vpc()
        .silo_id(silo.id)
        .project_id(proj_b.id)
        .vpc_id(vpc_in_a.id)
        .send()
        .await
        .expect_err("cross-project delete must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_vpc_name_in_different_projects_does_not_conflict() {
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
    let proj_a = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "a".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let proj_b = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "b".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    root.create_project_vpc()
        .silo_id(silo.id)
        .project_id(proj_a.id)
        .body(ipv4_only("shared", "10.0.0.0/24"))
        .send()
        .await
        .expect("create in proj_a");
    root.create_project_vpc()
        .silo_id(silo.id)
        .project_id(proj_b.id)
        .body(ipv4_only("shared", "10.1.0.0/24"))
        .send()
        .await
        .expect("same vpc name in a different project must succeed");

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vpc_with_no_cidr_returns_400() {
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
    let proj = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .create_project_vpc()
        .silo_id(silo.id)
        .project_id(proj.id)
        .body(NewVpc {
            name: "nothing".to_string(),
            description: None,
            ipv4_block: None,
            ipv6_block: None,
        })
        .send()
        .await
        .expect_err("vpc with no cidr must be 400");
    assert_status(err, 400);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vpc_under_unknown_project_returns_404() {
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

    // Make-believe project id, no project actually exists in this silo.
    let fake_project = Uuid::new_v4();
    let err = root
        .create_project_vpc()
        .silo_id(silo.id)
        .project_id(fake_project)
        .body(dual_stack("ghost"))
        .send()
        .await
        .expect_err("create under unknown project must 404");
    assert_status(err, 404);

    let err = root
        .list_project_vpcs()
        .silo_id(silo.id)
        .project_id(fake_project)
        .send()
        .await
        .expect_err("list under unknown project must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_cannot_reach_vpc_endpoints() {
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
    let proj = root
        .create_silo_project()
        .silo_id(silo.id)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let anon = test.anonymous_client();
    let err = anon
        .list_project_vpcs()
        .silo_id(silo.id)
        .project_id(proj.id)
        .send()
        .await
        .expect_err("anonymous list must be denied");
    assert_status(err, 404);

    let err = anon
        .create_project_vpc()
        .silo_id(silo.id)
        .project_id(proj.id)
        .body(dual_stack("forbidden"))
        .send()
        .await
        .expect_err("anonymous create must be denied");
    assert_status(err, 404);

    test.close().await;
}
