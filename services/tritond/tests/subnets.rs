// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v2/silos/{silo_id}/projects/{project_id}/vpcs/{vpc_id}/subnets`
//! surface.
//!
//! Mirrors `tests/vpcs.rs`: same in-process tritond fixture + stub
//! OIDC. New invariants exercised here, on top of the cross-tenant
//! 404 story:
//!
//! * Subnet CIDRs must be contained in the parent VPC's same-family
//!   block; mismatch → 409.
//! * Subnet CIDRs must not overlap peer subnet CIDRs in the same
//!   VPC; overlap → 409.
//! * Subnet IP family must be present on the parent VPC (no IPv4
//!   subnet in an IPv6-only VPC).
//! * Cross-VPC defence-in-depth: a subnet_id in path /vpcs/A but
//!   actually under VPC B → 404.
//! * Deleting a VPC with attached subnets → 409 (no Phase-0
//!   cascade).

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
use tritond_client::types::{NewIdpConfig, NewProject, NewSilo, NewSubnet, NewVpc};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "subnets-test";

// ---------- stub OIDC provider ----------

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

/// Convenience: create silo + project + a dual-stack VPC with a
/// /16 IPv4 block and /48 IPv6 block. Returns `(silo_id, project_id,
/// vpc_id)`.
async fn make_silo_project_vpc(root: &tritond_client::Client) -> (Uuid, Uuid, Uuid) {
    let silo = root
        .create_silo()
        .body(NewSilo {
            name: format!("silo-{}", Uuid::new_v4()),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let project = root
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
    let vpc = root
        .create_project_vpc()
        .silo_id(silo.id)
        .project_id(project.id)
        .body(NewVpc {
            name: "vpc1".to_string(),
            description: None,
            ipv4_block: Some("10.0.0.0/16".to_string()),
            ipv6_block: Some("fd00::/48".to_string()),
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    (silo.id, project.id, vpc.id)
}

fn dual_stack_subnet(name: &str, v4: &str, v6: &str) -> NewSubnet {
    NewSubnet {
        name: name.to_string(),
        description: None,
        ipv4_block: Some(v4.to_string()),
        ipv6_block: Some(v6.to_string()),
    }
}

fn ipv4_only_subnet(name: &str, v4: &str) -> NewSubnet {
    NewSubnet {
        name: name.to_string(),
        description: None,
        ipv4_block: Some(v4.to_string()),
        ipv6_block: None,
    }
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn root_can_create_and_read_subnets_in_any_vpc() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id, vpc_id) = make_silo_project_vpc(&root).await;

    let subnet = root
        .create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(dual_stack_subnet("web", "10.0.1.0/24", "fd00:0:0:1::/64"))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(subnet.silo_id, silo_id);
    assert_eq!(subnet.project_id, project_id);
    assert_eq!(subnet.vpc_id, vpc_id);
    assert_eq!(subnet.ipv4_block.as_deref(), Some("10.0.1.0/24"));
    assert_eq!(subnet.ipv6_block.as_deref(), Some("fd00:0:0:1::/64"));

    let listed = root
        .list_vpc_subnets()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, subnet.id);

    let fetched = root
        .get_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .subnet_id(subnet.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, subnet.id);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subnet_cidr_outside_vpc_block_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id, vpc_id) = make_silo_project_vpc(&root).await;

    // VPC's ipv4_block is 10.0.0.0/16. 10.1.0.0/24 is outside.
    let err = root
        .create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(ipv4_only_subnet("out", "10.1.0.0/24"))
        .send()
        .await
        .expect_err("subnet outside vpc block must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overlapping_subnet_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id, vpc_id) = make_silo_project_vpc(&root).await;

    root.create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(ipv4_only_subnet("first", "10.0.0.0/24"))
        .send()
        .await
        .unwrap();

    let err = root
        .create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(ipv4_only_subnet("overlap", "10.0.0.128/25"))
        .send()
        .await
        .expect_err("overlapping subnet must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ipv4_subnet_in_ipv6_only_vpc_returns_409() {
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
    let project = root
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
    let vpc = root
        .create_project_vpc()
        .silo_id(silo.id)
        .project_id(project.id)
        .body(NewVpc {
            name: "v6only".to_string(),
            description: None,
            ipv4_block: None,
            ipv6_block: Some("fd00::/48".to_string()),
        })
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .create_vpc_subnet()
        .silo_id(silo.id)
        .project_id(project.id)
        .vpc_id(vpc.id)
        .body(ipv4_only_subnet("wrong-family", "10.0.0.0/24"))
        .send()
        .await
        .expect_err("ipv4 subnet in ipv6-only vpc must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subnet_with_no_cidr_returns_400() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id, vpc_id) = make_silo_project_vpc(&root).await;

    let err = root
        .create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(NewSubnet {
            name: "nothing".to_string(),
            description: None,
            ipv4_block: None,
            ipv6_block: None,
        })
        .send()
        .await
        .expect_err("subnet with no cidr must 400");
    assert_status(err, 400);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_subnet_name_within_vpc_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id, vpc_id) = make_silo_project_vpc(&root).await;

    root.create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(ipv4_only_subnet("alpha", "10.0.1.0/24"))
        .send()
        .await
        .unwrap();
    let err = root
        .create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(ipv4_only_subnet("alpha", "10.0.2.0/24"))
        .send()
        .await
        .expect_err("duplicate subnet name within vpc must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_vpc_subnet_get_returns_404() {
    // Two VPCs in the same project. A subnet lives in VPC A. GET via
    // VPC B's path must 404 (defence-in-depth on vpc_id, even though
    // Cedar would otherwise allow same-silo same-project access).
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id, vpc_a) = make_silo_project_vpc(&root).await;
    let vpc_b = root
        .create_project_vpc()
        .silo_id(silo_id)
        .project_id(project_id)
        .body(NewVpc {
            name: "vpc2".to_string(),
            description: None,
            ipv4_block: Some("10.1.0.0/16".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let subnet = root
        .create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_a)
        .body(ipv4_only_subnet("net", "10.0.0.0/24"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .get_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_b.id) // wrong vpc for this subnet
        .subnet_id(subnet.id)
        .send()
        .await
        .expect_err("cross-vpc get must 404");
    assert_status(err, 404);

    let err = root
        .delete_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_b.id)
        .subnet_id(subnet.id)
        .send()
        .await
        .expect_err("cross-vpc delete must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_vpc_with_subnets_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id, vpc_id) = make_silo_project_vpc(&root).await;
    let subnet = root
        .create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(ipv4_only_subnet("occupant", "10.0.1.0/24"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .delete_project_vpc()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .expect_err("delete vpc with subnets must 409");
    assert_status(err, 409);

    // Clear the subnet, then the VPC delete succeeds.
    root.delete_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .subnet_id(subnet.id)
        .send()
        .await
        .unwrap();
    root.delete_project_vpc()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .unwrap();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn federated_user_cross_silo_subnet_create_returns_404() {
    // Alpha tenant (federated) tries to create a subnet under beta's
    // VPC. Cross-silo Cedar deny → 404.
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
        .body(NewVpc {
            name: "betav".to_string(),
            description: None,
            ipv4_block: Some("10.0.0.0/16".to_string()),
            ipv6_block: None,
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

    let err = tenant
        .create_vpc_subnet()
        .silo_id(silo_beta.id)
        .project_id(beta_proj.id)
        .vpc_id(beta_vpc.id)
        .body(ipv4_only_subnet("intruder", "10.0.0.0/24"))
        .send()
        .await
        .expect_err("alpha-tenant must not create in beta");
    assert_status(err, 404);

    let _ = shutdown.send(());
    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_cannot_reach_subnet_endpoints() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (silo_id, project_id, vpc_id) = make_silo_project_vpc(&root).await;

    let anon = test.anonymous_client();
    let err = anon
        .list_vpc_subnets()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .expect_err("anonymous list must be denied");
    assert_status(err, 404);

    test.close().await;
}
