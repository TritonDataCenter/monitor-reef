// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the
//! `/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/route-tables/{route_table_id}/routes`
//! surface.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password, mint_access};
use tritond_client::types::{
    AddressFamily, NewNatGateway, NewProject, NewRoute, NewRouteTable, NewSilo, NewVpc, RouteTarget,
};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const ROOT_PASSWORD: &str = "route-test";

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

fn new_route(name: &str, destination: &str, target: RouteTarget) -> NewRoute {
    NewRoute {
        name: name.to_string(),
        description: None,
        destination: destination.to_string(),
        target,
    }
}

fn new_nat_gateway(name: &str) -> NewNatGateway {
    NewNatGateway {
        name: name.to_string(),
        description: None,
        family: AddressFamily::V4,
    }
}

async fn make_project_vpc(
    root: &tritond_client::Client,
    silo_name: &str,
    vpc_name: &str,
    ipv4_block: &str,
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
        .create_project_vpc()
        .tenant_id(silo.default_tenant_id)
        .project_id(project.id)
        .body(NewVpc {
            name: vpc_name.to_string(),
            description: None,
            ipv4_block: Some(ipv4_block.to_string()),
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
async fn root_can_create_list_get_delete_nat_route() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, route_table_id) =
        make_project_vpc(&root, "alpha", "v", "10.0.0.0/16").await;
    let nat = root
        .create_vpc_nat_gateway()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(new_nat_gateway("egress"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let route = root
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .body(new_route(
            "default-egress",
            "0.0.0.0/0",
            RouteTarget::NatGateway {
                nat_gateway_id: nat.id,
            },
        ))
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(route.tenant_id, tenant_id);
    assert_eq!(route.project_id, project_id);
    assert_eq!(route.vpc_id, vpc_id);
    assert_eq!(route.route_table_id, route_table_id);
    assert_eq!(route.destination, "0.0.0.0/0");

    let listed = root
        .list_vpc_route_table_routes()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, route.id);

    let fetched = root
        .get_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .route_id(route.id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.id, route.id);

    root.delete_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .route_id(route.id)
        .send()
        .await
        .unwrap();
    let err = root
        .get_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .route_id(route.id)
        .send()
        .await
        .expect_err("deleted route must 404");
    assert_status(err, 404);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_destination_in_table_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, route_table_id) =
        make_project_vpc(&root, "dupe", "v", "10.0.0.0/16").await;

    root.create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .body(new_route(
            "blackhole-a",
            "10.9.0.0/24",
            RouteTarget::Blackhole,
        ))
        .send()
        .await
        .unwrap();
    let err = root
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .body(new_route("blackhole-b", "10.9.0.0/24", RouteTarget::Reject))
        .send()
        .await
        .expect_err("duplicate route destination must 409");
    assert_status(err, 409);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nat_target_in_different_vpc_returns_400() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_a, route_table_a) =
        make_project_vpc(&root, "cross-target", "a", "10.0.0.0/16").await;
    let vpc_b = root
        .create_project_vpc()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .body(NewVpc {
            name: "b".to_string(),
            description: None,
            ipv4_block: Some("10.1.0.0/16".to_string()),
            ipv6_block: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let nat_b = root
        .create_vpc_nat_gateway()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_b.id)
        .body(new_nat_gateway("egress-b"))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_a)
        .route_table_id(route_table_a)
        .body(new_route(
            "bad-egress",
            "0.0.0.0/0",
            RouteTarget::NatGateway {
                nat_gateway_id: nat_b.id,
            },
        ))
        .send()
        .await
        .expect_err("cross-vpc NAT route target must 400");
    assert_status(err, 400);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nat_gateway_delete_with_referencing_route_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, route_table_id) =
        make_project_vpc(&root, "nat-delete", "v", "10.0.0.0/16").await;
    let nat = root
        .create_vpc_nat_gateway()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(new_nat_gateway("egress"))
        .send()
        .await
        .unwrap()
        .into_inner();
    let route = root
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .body(new_route(
            "default-egress",
            "0.0.0.0/0",
            RouteTarget::NatGateway {
                nat_gateway_id: nat.id,
            },
        ))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .delete_vpc_nat_gateway()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .nat_gateway_id(nat.id)
        .send()
        .await
        .expect_err("referenced NAT gateway delete must 409");
    assert_status(err, 409);

    root.delete_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .route_id(route.id)
        .send()
        .await
        .unwrap();
    root.delete_vpc_nat_gateway()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .nat_gateway_id(nat.id)
        .send()
        .await
        .unwrap();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn route_table_delete_with_routes_returns_409() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, _) =
        make_project_vpc(&root, "rt-delete", "v", "10.0.0.0/16").await;
    let route_table = root
        .create_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(NewRouteTable {
            name: "egress".to_string(),
            description: None,
        })
        .send()
        .await
        .unwrap()
        .into_inner();
    let route = root
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table.id)
        .body(new_route(
            "blackhole",
            "10.99.0.0/24",
            RouteTarget::Blackhole,
        ))
        .send()
        .await
        .unwrap()
        .into_inner();

    let err = root
        .delete_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table.id)
        .send()
        .await
        .expect_err("route table with routes must 409");
    assert_status(err, 409);

    root.delete_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table.id)
        .route_id(route.id)
        .send()
        .await
        .unwrap();
    root.delete_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table.id)
        .send()
        .await
        .unwrap();

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn floating_ip_route_target_is_rejected_at_api_edge() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, route_table_id) =
        make_project_vpc(&root, "fip-target", "v", "10.0.0.0/16").await;

    let err = root
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .body(new_route(
            "system-only",
            "10.88.0.0/24",
            RouteTarget::FloatingIp {
                floating_ip_id: Uuid::new_v4(),
            },
        ))
        .send()
        .await
        .expect_err("floating ip target must 400");
    assert_status(err, 400);

    test.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_cannot_reach_route_endpoints() {
    let test = TestServer::start().await;
    let root = test.root_client();
    let (tenant_id, project_id, vpc_id, route_table_id) =
        make_project_vpc(&root, "anon", "v", "10.0.0.0/16").await;
    let route = root
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .body(new_route(
            "blackhole",
            "10.77.0.0/24",
            RouteTarget::Blackhole,
        ))
        .send()
        .await
        .unwrap()
        .into_inner();
    let anon = test.anonymous_client();

    let err = anon
        .list_vpc_route_table_routes()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .send()
        .await
        .expect_err("anonymous list must be denied");
    assert_status(err, 404);

    let err = anon
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .body(new_route(
            "forbidden",
            "10.78.0.0/24",
            RouteTarget::Blackhole,
        ))
        .send()
        .await
        .expect_err("anonymous create must be denied");
    assert_status(err, 404);

    let err = anon
        .get_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .route_id(route.id)
        .send()
        .await
        .expect_err("anonymous get must be denied");
    assert_status(err, 404);

    let err = anon
        .delete_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .route_id(route.id)
        .send()
        .await
        .expect_err("anonymous delete must be denied");
    assert_status(err, 404);

    test.close().await;
}
