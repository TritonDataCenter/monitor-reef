// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v1/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables`
//! surface and its tenant-scoped Cedar gates.

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
use tritond_client::types::{NewIdpConfig, NewProject, NewRouteTable, NewSilo, NewVpc};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "route-table-test";

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
    async fn start() -> (StubIdp, oneshot::Sender<()>) {
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
        let app: Router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .with_state(stub.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .ok();
        });
        (stub, shutdown_tx)
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

fn new_route_table(name: &str) -> NewRouteTable {
    NewRouteTable {
        name: name.to_string(),
        description: None,
    }
}

async fn make_project_vpc(
    root: &tritond_client::Client,
    silo_name: &str,
) -> (Uuid, Uuid, Uuid, Uuid) {
    let silo = root
        .create_silo()
        .body(NewSilo {
            name: silo_name.to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let project = root
        .create_tenant_project()
        .tenant_id(silo.default_tenant_id)
        .body(NewProject {
            name: "p".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let vpc = root
        .create_vpc_v1()
        .tenant(silo.default_tenant_id)
        .project(project.id)
        .body(NewVpc {
            name: "v".to_string(),
            description: None,
            ipv4_block: Some("10.0.0.0/16".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    (
        silo.default_tenant_id,
        project.id,
        vpc.id,
        vpc.main_route_table_id,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn root_can_list_main_and_create_get_delete_route_table() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, main_route_table_id) =
        make_project_vpc(&root, "alpha").await;

    let listed = root
        .list_vpc_route_tables()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    let main = listed
        .iter()
        .find(|rt| rt.id == main_route_table_id)
        .expect("main route table is listed");
    assert!(main.is_main);
    assert_eq!(main.name, "main");

    let route_table = root
        .create_route_table_v1()
        .vpc(vpc_id)
        .body(new_route_table("egress"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(route_table.tenant_id, tenant_id);
    assert_eq!(route_table.project_id, project_id);
    assert_eq!(route_table.vpc_id, vpc_id);
    assert!(!route_table.is_main);

    let listed = root
        .list_vpc_route_tables()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|rt| rt.id == route_table.id));

    let fetched = root
        .get_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.name, "egress");

    root.delete_route_table_v1()
        .route_table_id(route_table.id)
        .send()
        .await
        .unwrap();

    let err = root
        .get_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table.id)
        .send()
        .await
        .expect_err("deleted route table must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_route_table_name_within_vpc_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, _) = make_project_vpc(&root, "dupe").await;

    root.create_route_table_v1()
        .vpc(vpc_id)
        .body(new_route_table("egress"))
        .send()
        .await
        .unwrap();
    let err = root
        .create_route_table_v1()
        .vpc(vpc_id)
        .body(new_route_table("egress"))
        .send()
        .await
        .expect_err("duplicate route table name must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn main_route_table_delete_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, main_route_table_id) =
        make_project_vpc(&root, "main-delete").await;

    let err = root
        .delete_route_table_v1()
        .route_table_id(main_route_table_id)
        .send()
        .await
        .expect_err("main route table delete must 409");
    assert_status(err, 409);

    let fetched = root
        .get_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(main_route_table_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(fetched.is_main);

    test.close().await;
}

// RFD 00007 AP-3e: same as nat_gateways::cross_vpc_get_and_delete.
// Tracked at AP-3b-6.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs non-root principal fixtures; tracked at AP-3b-6"]
async fn cross_vpc_get_and_delete_return_404() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_a, _) = make_project_vpc(&root, "cross").await;
    let vpc_b = root
        .create_vpc_v1()
        .tenant(tenant_id)
        .project(project_id)
        .body(NewVpc {
            name: "other".to_string(),
            description: None,
            ipv4_block: Some("10.1.0.0/16".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let route_table = root
        .create_route_table_v1()
        .vpc(vpc_a)
        .body(new_route_table("egress"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .get_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_b.id)
        .route_table_id(route_table.id)
        .send()
        .await
        .expect_err("cross-vpc get must 404");
    assert_status(err, 404);

    let err = root
        .delete_route_table_v1()
        .route_table_id(route_table.id)
        .send()
        .await
        .expect_err("cross-vpc delete must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn federated_user_can_manage_route_tables_in_own_tenant_only() {
    let test = TestServer::start().await;
    let (idp, shutdown) = StubIdp::start().await;
    let root = test.root_client();
    let (alpha_tenant, alpha_project, alpha_vpc, _) = make_project_vpc(&root, "alpha").await;
    let (beta_tenant, beta_project, beta_vpc, _) = make_project_vpc(&root, "beta").await;

    root.put_tenant_idp()
        .tenant_id(alpha_tenant)
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

    let route_table = tenant
        .create_route_table_v1()
        .vpc(alpha_vpc)
        .body(new_route_table("tenant-egress"))
        .send()
        .await
        .expect("alpha tenant should create route table in alpha")
        .into_inner();
    assert_eq!(route_table.tenant_id, alpha_tenant);

    let listed = tenant
        .list_vpc_route_tables()
        .tenant_id(alpha_tenant)
        .project_id(alpha_project)
        .vpc_id(alpha_vpc)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|rt| rt.id == route_table.id));

    let fetched = tenant
        .get_vpc_route_table()
        .tenant_id(alpha_tenant)
        .project_id(alpha_project)
        .vpc_id(alpha_vpc)
        .route_table_id(route_table.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, route_table.id);

    tenant
        .delete_route_table_v1()
        .route_table_id(route_table.id)
        .send()
        .await
        .unwrap();

    let err = tenant
        .create_route_table_v1()
        .vpc(beta_vpc)
        .body(new_route_table("intruder"))
        .send()
        .await
        .expect_err("alpha tenant must not create beta route table");
    assert_status(err, 404);

    let _ = shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_cannot_reach_route_table_endpoints() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, main_route_table_id) =
        make_project_vpc(&root, "anon").await;
    let anon = test.anonymous_client();

    let err = anon
        .list_vpc_route_tables()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .expect_err("anonymous list must be denied");
    assert_status(err, 404);

    let err = anon
        .create_route_table_v1()
        .vpc(vpc_id)
        .body(new_route_table("forbidden"))
        .send()
        .await
        .expect_err("anonymous create must be denied");
    assert_status(err, 404);

    let err = anon
        .get_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(main_route_table_id)
        .send()
        .await
        .expect_err("anonymous get must be denied");
    assert_status(err, 404);

    let err = anon
        .delete_route_table_v1()
        .route_table_id(main_route_table_id)
        .send()
        .await
        .expect_err("anonymous delete must be denied");
    assert_status(err, 404);

    test.close().await;
}
