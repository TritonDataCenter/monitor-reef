// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The `TritondApi` trait implementation: every HTTP handler, the
//! cross-scope visibility helpers they share, the `ApiDescription`
//! builder, and the Dropshot server bootstrap. Helpers that don't
//! belong to a single handler family live in the sibling modules
//! (`error`, `principal`, `validate`, `lifecycle`, `cn_credential`,
//! `blueprint`, `edge_cluster`, `bundle`).

use crate::error::*;
use crate::validate::*;
use crate::{dhcp_reconciler, provisioner, sweeper};

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use dropshot::{
    ApiDescription, ClientErrorStatusCode, ConfigDropshot, ConfigLogging, ConfigLoggingLevel,
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    HttpResponseUpdatedNoContent, HttpServer, HttpServerStarter, Path, Query, RequestContext,
    TypedBody,
};
use tritond_api::{
    AgentJobPath, AgentPortBlueprint, AgentPortBlueprintPath, AgentStatusRequest, ApiKeyCreated,
    ApiKeyPath, ApproveCnRequest, AttachFloatingIpRequest, AuditEventList, AuditEventPath,
    AuditListQuery, AuditVerifyQuery, AuditVerifyResponse, ClaimJobRequest, ClaimJobResponse,
    CnListQuery, CnPath, CompleteJobRequest, ConfigEntry, ConfigKeyPath, HealthResponse, ImagePath,
    InstanceDeleteQuery, InstanceLogsPath, LegacyCnSummary, LegacyVmListQuery, LegacyVmPath,
    LogTailQuery, LoginRequest, MetricsRangeQuery, NetworkRealizationRequest, NewApiKey,
    NewIdpConfig, NewImageFromBundle, OpenAutoApproveRequest, ProvisioningBlueprint,
    RefreshRequest, RegisterCnRequest, RegisterCnResponse, RegisterStatusQuery,
    RegisterStatusResponse, SetCnRoleRequest, SetConfigRequest, SiloPath, SiloTenantPath,
    SshKeyPath, StorageClusterAccessKeyPath, StorageClusterBucketPath, StorageClusterNodePath,
    StorageClusterPath, StorageClusterUserPath, StorageClusterUserPolicyPath, TenantIdpPath,
    TenantPath, TenantProjectFloatingIpPath, TenantProjectInstanceDiskPath,
    TenantProjectInstanceNicPath, TenantProjectInstancePath, TenantProjectPath,
    TenantProjectVpcDhcpMacPath, TenantProjectVpcFirewallRulePath, TenantProjectVpcNatGatewayPath,
    TenantProjectVpcPath, TenantProjectVpcRouteTablePath, TenantProjectVpcRouteTableRoutePath,
    TenantProjectVpcSubnetPath, TokenResponse, TritondApi,
    types::{
        ApiKeyView, AuditEvent, AutoApproveWindow, CnView, DhcpLease, DhcpPool, DhcpReservation,
        Disk, FirewallRule, FloatingIp, IdpConfigView, Image, Instance, LegacyVm, NatGateway,
        NewDhcpPool, NewDhcpReservation, NewFirewallRule, NewFloatingIp, NewImage, NewInstance,
        NewNatGateway, NewProject, NewQuota, NewRoute, NewRouteTable, NewSilo, NewSshKey,
        NewStorageCluster, NewSubnet, NewTenant, NewVpc, Nic, PresignGetRequest, PresignPutRequest,
        PresignResponse, Project, ProvisioningJob, Quota, Route, RouteTable, SetPresignerRequest,
        Silo, SshKey, StorageAccessKey, StorageBucket, StorageClusterSummary, StorageClusterView,
        StorageMembership, StorageNode, StorageObjectsPage, StorageUser, Subnet, Tenant, Vpc,
    },
};
use tritond_audit::Outcome as AuditOutcome;
use tritond_auth::{mint_access, mint_refresh};
use tritond_store::{ConfigKey, StoreError};
use uuid::Uuid;

use crate::auth::{Action, AuthService, Principal};

use crate::context::ApiContext;

/// Concrete implementor of [`TritondApi`].
pub enum TritondServiceImpl {}

impl TritondApi for TritondServiceImpl {
    type Context = ApiContext;

    async fn health(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError> {
        crate::handlers::health::health(rqctx).await
    }

    async fn create_silo(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSilo>,
    ) -> Result<HttpResponseCreated<Silo>, HttpError> {
        crate::handlers::silos::create_silo(rqctx, body).await
    }

    async fn list_silos(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Silo>>, HttpError> {
        crate::handlers::silos::list_silos(rqctx).await
    }

    async fn get_silo(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Silo>, HttpError> {
        crate::handlers::silos::get_silo(rqctx, path).await
    }

    async fn login(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError> {
        crate::handlers::auth_keys::login(rqctx, body).await
    }

    async fn refresh(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RefreshRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError> {
        crate::handlers::auth_keys::refresh(rqctx, body).await
    }

    async fn create_api_key(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewApiKey>,
    ) -> Result<HttpResponseCreated<ApiKeyCreated>, HttpError> {
        crate::handlers::auth_keys::create_api_key(rqctx, body).await
    }

    async fn list_api_keys(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<ApiKeyView>>, HttpError> {
        crate::handlers::auth_keys::list_api_keys(rqctx).await
    }

    async fn delete_api_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<ApiKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::auth_keys::delete_api_key(rqctx, path).await
    }

    async fn list_audit_events(
        rqctx: RequestContext<Self::Context>,
        query: Query<AuditListQuery>,
    ) -> Result<HttpResponseOk<AuditEventList>, HttpError> {
        crate::handlers::auth_keys::list_audit_events(rqctx, query).await
    }

    async fn get_audit_event(
        rqctx: RequestContext<Self::Context>,
        path: Path<AuditEventPath>,
    ) -> Result<HttpResponseOk<AuditEvent>, HttpError> {
        crate::handlers::auth_keys::get_audit_event(rqctx, path).await
    }

    async fn verify_audit_chain(
        rqctx: RequestContext<Self::Context>,
        query: Query<AuditVerifyQuery>,
    ) -> Result<HttpResponseOk<AuditVerifyResponse>, HttpError> {
        crate::handlers::auth_keys::verify_audit_chain(rqctx, query).await
    }

    async fn agent_claim_job(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<ClaimJobRequest>,
    ) -> Result<HttpResponseOk<ClaimJobResponse>, HttpError> {
        crate::handlers::agents::agent_claim_job(rqctx, body).await
    }

    async fn agent_job_blueprint(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentJobPath>,
    ) -> Result<HttpResponseOk<ProvisioningBlueprint>, HttpError> {
        crate::handlers::agents::agent_job_blueprint(rqctx, path).await
    }

    async fn agent_port_blueprint(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentPortBlueprintPath>,
    ) -> Result<HttpResponseOk<AgentPortBlueprint>, HttpError> {
        crate::handlers::agents::agent_port_blueprint(rqctx, path).await
    }

    async fn agent_complete_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<AgentJobPath>,
        body: TypedBody<CompleteJobRequest>,
    ) -> Result<HttpResponseOk<ProvisioningJob>, HttpError> {
        crate::handlers::agents::agent_complete_job(rqctx, path, body).await
    }

    async fn put_tenant_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantIdpPath>,
        body: TypedBody<NewIdpConfig>,
    ) -> Result<HttpResponseCreated<IdpConfigView>, HttpError> {
        crate::handlers::tenants::put_tenant_idp(rqctx, path, body).await
    }

    async fn get_tenant_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantIdpPath>,
    ) -> Result<HttpResponseOk<IdpConfigView>, HttpError> {
        crate::handlers::tenants::get_tenant_idp(rqctx, path).await
    }

    async fn delete_tenant_idp(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantIdpPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::tenants::delete_tenant_idp(rqctx, path).await
    }

    async fn list_silo_tenants(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<Tenant>>, HttpError> {
        crate::handlers::tenants::list_silo_tenants(rqctx, path).await
    }

    async fn create_silo_tenant(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewTenant>,
    ) -> Result<HttpResponseCreated<Tenant>, HttpError> {
        crate::handlers::tenants::create_silo_tenant(rqctx, path, body).await
    }

    async fn get_silo_tenant(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloTenantPath>,
    ) -> Result<HttpResponseOk<Tenant>, HttpError> {
        crate::handlers::tenants::get_silo_tenant(rqctx, path).await
    }

    async fn delete_silo_tenant(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloTenantPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::tenants::delete_silo_tenant(rqctx, path).await
    }

    async fn list_tenant_projects(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
    ) -> Result<HttpResponseOk<Vec<Project>>, HttpError> {
        crate::handlers::tenants::list_tenant_projects(rqctx, path).await
    }

    async fn create_tenant_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
        body: TypedBody<NewProject>,
    ) -> Result<HttpResponseCreated<Project>, HttpError> {
        crate::handlers::tenants::create_tenant_project(rqctx, path, body).await
    }

    async fn get_tenant_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Project>, HttpError> {
        crate::handlers::projects::get_tenant_project(rqctx, path).await
    }

    async fn delete_tenant_project(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::projects::delete_tenant_project(rqctx, path).await
    }

    async fn list_project_vpcs(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Vpc>>, HttpError> {
        crate::handlers::network::vpc::list_project_vpcs(rqctx, path).await
    }

    async fn create_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewVpc>,
    ) -> Result<HttpResponseCreated<Vpc>, HttpError> {
        crate::handlers::network::vpc::create_project_vpc(rqctx, path, body).await
    }

    async fn get_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vpc>, HttpError> {
        crate::handlers::network::vpc::get_project_vpc(rqctx, path).await
    }

    async fn delete_project_vpc(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::vpc::delete_project_vpc(rqctx, path).await
    }

    async fn list_vpc_subnets(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<Subnet>>, HttpError> {
        crate::handlers::network::subnet::list_vpc_subnets(rqctx, path).await
    }

    async fn create_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewSubnet>,
    ) -> Result<HttpResponseCreated<Subnet>, HttpError> {
        crate::handlers::network::subnet::create_vpc_subnet(rqctx, path, body).await
    }

    async fn get_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcSubnetPath>,
    ) -> Result<HttpResponseOk<Subnet>, HttpError> {
        crate::handlers::network::subnet::get_vpc_subnet(rqctx, path).await
    }

    async fn delete_vpc_subnet(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcSubnetPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::subnet::delete_vpc_subnet(rqctx, path).await
    }

    async fn list_vpc_route_tables(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<RouteTable>>, HttpError> {
        crate::handlers::network::routes::list_vpc_route_tables(rqctx, path).await
    }

    async fn create_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewRouteTable>,
    ) -> Result<HttpResponseCreated<RouteTable>, HttpError> {
        crate::handlers::network::routes::create_vpc_route_table(rqctx, path, body).await
    }

    async fn get_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseOk<RouteTable>, HttpError> {
        crate::handlers::network::routes::get_vpc_route_table(rqctx, path).await
    }

    async fn delete_vpc_route_table(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::routes::delete_vpc_route_table(rqctx, path).await
    }

    async fn list_vpc_route_table_routes(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
    ) -> Result<HttpResponseOk<Vec<Route>>, HttpError> {
        crate::handlers::network::routes::list_vpc_route_table_routes(rqctx, path).await
    }

    async fn create_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTablePath>,
        body: TypedBody<NewRoute>,
    ) -> Result<HttpResponseCreated<Route>, HttpError> {
        crate::handlers::network::routes::create_vpc_route_table_route(rqctx, path, body).await
    }

    async fn get_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTableRoutePath>,
    ) -> Result<HttpResponseOk<Route>, HttpError> {
        crate::handlers::network::routes::get_vpc_route_table_route(rqctx, path).await
    }

    async fn delete_vpc_route_table_route(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcRouteTableRoutePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::routes::delete_vpc_route_table_route(rqctx, path).await
    }

    // ---- Firewall rules (Slice 1: per-VPC flat rule list) ----------

    async fn list_vpc_firewall_rules(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<FirewallRule>>, HttpError> {
        crate::handlers::network::firewall::list_vpc_firewall_rules(rqctx, path).await
    }

    async fn create_vpc_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewFirewallRule>,
    ) -> Result<HttpResponseCreated<FirewallRule>, HttpError> {
        crate::handlers::network::firewall::create_vpc_firewall_rule(rqctx, path, body).await
    }

    async fn delete_vpc_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcFirewallRulePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::firewall::delete_vpc_firewall_rule(rqctx, path).await
    }

    // ---- DHCP / IPAM (γ.1 + γ.4) -----------------------------------

    async fn get_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Option<DhcpPool>>, HttpError> {
        crate::handlers::network::dhcp::get_vpc_dhcp_pool(rqctx, path).await
    }

    async fn set_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewDhcpPool>,
    ) -> Result<HttpResponseOk<DhcpPool>, HttpError> {
        crate::handlers::network::dhcp::set_vpc_dhcp_pool(rqctx, path, body).await
    }

    async fn clear_vpc_dhcp_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::dhcp::clear_vpc_dhcp_pool(rqctx, path).await
    }

    async fn list_vpc_dhcp_reservations(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<DhcpReservation>>, HttpError> {
        crate::handlers::network::dhcp::list_vpc_dhcp_reservations(rqctx, path).await
    }

    async fn create_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewDhcpReservation>,
    ) -> Result<HttpResponseCreated<DhcpReservation>, HttpError> {
        crate::handlers::network::dhcp::create_vpc_dhcp_reservation(rqctx, path, body).await
    }

    async fn get_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseOk<DhcpReservation>, HttpError> {
        crate::handlers::network::dhcp::get_vpc_dhcp_reservation(rqctx, path).await
    }

    async fn delete_vpc_dhcp_reservation(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::dhcp::delete_vpc_dhcp_reservation(rqctx, path).await
    }

    async fn list_vpc_dhcp_leases(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<DhcpLease>>, HttpError> {
        crate::handlers::network::dhcp::list_vpc_dhcp_leases(rqctx, path).await
    }

    async fn get_vpc_dhcp_lease(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseOk<DhcpLease>, HttpError> {
        crate::handlers::network::dhcp::get_vpc_dhcp_lease(rqctx, path).await
    }

    async fn delete_vpc_dhcp_lease(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcDhcpMacPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::dhcp::delete_vpc_dhcp_lease(rqctx, path).await
    }

    async fn list_vpc_nat_gateways(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
    ) -> Result<HttpResponseOk<Vec<NatGateway>>, HttpError> {
        crate::handlers::network::nat::list_vpc_nat_gateways(rqctx, path).await
    }

    async fn create_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcPath>,
        body: TypedBody<NewNatGateway>,
    ) -> Result<HttpResponseCreated<NatGateway>, HttpError> {
        crate::handlers::network::nat::create_vpc_nat_gateway(rqctx, path, body).await
    }

    async fn get_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcNatGatewayPath>,
    ) -> Result<HttpResponseOk<NatGateway>, HttpError> {
        crate::handlers::network::nat::get_vpc_nat_gateway(rqctx, path).await
    }

    async fn delete_vpc_nat_gateway(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectVpcNatGatewayPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::network::nat::delete_vpc_nat_gateway(rqctx, path).await
    }

    async fn list_public_ssh_keys(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
        crate::handlers::ssh_keys::list_public_ssh_keys(rqctx).await
    }

    async fn create_public_ssh_key(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError> {
        crate::handlers::ssh_keys::create_public_ssh_key(rqctx, body).await
    }

    async fn list_silo_ssh_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
        crate::handlers::ssh_keys::list_silo_ssh_keys(rqctx, path).await
    }

    async fn create_silo_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError> {
        crate::handlers::ssh_keys::create_silo_ssh_key(rqctx, path, body).await
    }

    async fn list_tenant_ssh_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
        crate::handlers::ssh_keys::list_tenant_ssh_keys(rqctx, path).await
    }

    async fn create_tenant_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError> {
        crate::handlers::ssh_keys::create_tenant_ssh_key(rqctx, path, body).await
    }

    async fn list_project_ssh_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
        crate::handlers::ssh_keys::list_project_ssh_keys(rqctx, path).await
    }

    async fn create_project_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError> {
        crate::handlers::ssh_keys::create_project_ssh_key(rqctx, path, body).await
    }

    async fn list_my_ssh_keys(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
        crate::handlers::ssh_keys::list_my_ssh_keys(rqctx).await
    }

    async fn create_my_ssh_key(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSshKey>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError> {
        crate::handlers::ssh_keys::create_my_ssh_key(rqctx, body).await
    }

    async fn get_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SshKeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError> {
        crate::handlers::ssh_keys::get_ssh_key(rqctx, path).await
    }

    async fn delete_ssh_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<SshKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::ssh_keys::delete_ssh_key(rqctx, path).await
    }

    async fn list_public_images(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
        crate::handlers::images::list_public_images(rqctx).await
    }

    async fn create_public_image(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError> {
        crate::handlers::images::create_public_image(rqctx, body).await
    }

    async fn list_silo_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
        crate::handlers::images::list_silo_images(rqctx, path).await
    }

    async fn create_silo_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError> {
        crate::handlers::images::create_silo_image(rqctx, path, body).await
    }

    async fn create_silo_image_from_bundle(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
        body: TypedBody<NewImageFromBundle>,
    ) -> Result<HttpResponseCreated<Image>, HttpError> {
        crate::handlers::images::create_silo_image_from_bundle(rqctx, path, body).await
    }

    async fn list_tenant_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
        crate::handlers::images::list_tenant_images(rqctx, path).await
    }

    async fn create_tenant_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantPath>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError> {
        crate::handlers::images::create_tenant_image(rqctx, path, body).await
    }

    async fn list_project_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
        crate::handlers::images::list_project_images(rqctx, path).await
    }

    async fn create_project_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError> {
        crate::handlers::images::create_project_image(rqctx, path, body).await
    }

    async fn list_my_images(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError> {
        crate::handlers::images::list_my_images(rqctx).await
    }

    async fn create_my_image(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewImage>,
    ) -> Result<HttpResponseCreated<Image>, HttpError> {
        crate::handlers::images::create_my_image(rqctx, body).await
    }

    async fn get_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
    ) -> Result<HttpResponseOk<Image>, HttpError> {
        crate::handlers::images::get_image(rqctx, path).await
    }

    async fn delete_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::images::delete_image(rqctx, path).await
    }

    async fn put_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewQuota>,
    ) -> Result<HttpResponseOk<Quota>, HttpError> {
        crate::handlers::projects::put_project_quota(rqctx, path, body).await
    }

    async fn get_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Quota>, HttpError> {
        crate::handlers::projects::get_project_quota(rqctx, path).await
    }

    async fn delete_project_quota(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::projects::delete_project_quota(rqctx, path).await
    }

    async fn list_project_instances(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<Instance>>, HttpError> {
        crate::handlers::instances::list_project_instances(rqctx, path).await
    }

    async fn create_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewInstance>,
    ) -> Result<HttpResponseCreated<Instance>, HttpError> {
        crate::handlers::instances::create_project_instance(rqctx, path, body).await
    }

    async fn get_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        crate::handlers::instances::get_project_instance(rqctx, path).await
    }

    async fn delete_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        query: Query<InstanceDeleteQuery>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::instances::delete_project_instance(rqctx, path, query).await
    }

    async fn start_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        crate::handlers::instances::start_project_instance(rqctx, path).await
    }

    async fn stop_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        crate::handlers::instances::stop_project_instance(rqctx, path).await
    }

    async fn restart_project_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError> {
        crate::handlers::instances::restart_project_instance(rqctx, path).await
    }

    async fn instance_console(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        query: Query<tritond_api::ConsoleQuery>,
        upgraded: dropshot::WebsocketConnection,
    ) -> dropshot::WebsocketChannelResult {
        crate::console::instance_console(rqctx, path, query, upgraded).await
    }

    async fn legacy_vm_console(
        rqctx: RequestContext<Self::Context>,
        path: Path<LegacyVmPath>,
        query: Query<tritond_api::ConsoleQuery>,
        upgraded: dropshot::WebsocketConnection,
    ) -> dropshot::WebsocketChannelResult {
        crate::console::legacy_vm_console(rqctx, path, query, upgraded).await
    }

    async fn list_instance_nics(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Nic>>, HttpError> {
        crate::handlers::instances::list_instance_nics(rqctx, path).await
    }

    async fn get_instance_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstanceNicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError> {
        crate::handlers::instances::get_instance_nic(rqctx, path).await
    }

    async fn list_instance_disks(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
    ) -> Result<HttpResponseOk<Vec<Disk>>, HttpError> {
        crate::handlers::instances::list_instance_disks(rqctx, path).await
    }

    async fn get_instance_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstanceDiskPath>,
    ) -> Result<HttpResponseOk<Disk>, HttpError> {
        crate::handlers::instances::get_instance_disk(rqctx, path).await
    }

    async fn list_project_floating_ips(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
    ) -> Result<HttpResponseOk<Vec<FloatingIp>>, HttpError> {
        crate::handlers::instances::list_project_floating_ips(rqctx, path).await
    }

    async fn create_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectPath>,
        body: TypedBody<NewFloatingIp>,
    ) -> Result<HttpResponseCreated<FloatingIp>, HttpError> {
        crate::handlers::instances::create_project_floating_ip(rqctx, path, body).await
    }

    async fn get_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        crate::handlers::instances::get_project_floating_ip(rqctx, path).await
    }

    async fn delete_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::instances::delete_project_floating_ip(rqctx, path).await
    }

    async fn attach_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
        body: TypedBody<AttachFloatingIpRequest>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        crate::handlers::instances::attach_project_floating_ip(rqctx, path, body).await
    }

    async fn detach_project_floating_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectFloatingIpPath>,
    ) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
        crate::handlers::instances::detach_project_floating_ip(rqctx, path).await
    }

    // ----- CN heartbeat / status (slice D) -----

    async fn agent_heartbeat(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<()>, HttpError> {
        crate::handlers::agents::agent_heartbeat(rqctx).await
    }

    async fn agent_status(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<AgentStatusRequest>,
    ) -> Result<HttpResponseOk<()>, HttpError> {
        crate::handlers::agents::agent_status(rqctx, body).await
    }

    async fn agent_report_network_realization(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NetworkRealizationRequest>,
    ) -> Result<HttpResponseOk<()>, HttpError> {
        crate::handlers::agents::agent_report_network_realization(rqctx, body).await
    }

    async fn agent_report_dhcp_lease_activity(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<tritond_api::DhcpLeaseActivityReport>,
    ) -> Result<HttpResponseOk<()>, HttpError> {
        crate::handlers::agents::agent_report_dhcp_lease_activity(rqctx, body).await
    }

    // ----- CN registration / approval (slice C) -----

    async fn agent_register(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RegisterCnRequest>,
    ) -> Result<HttpResponseOk<RegisterCnResponse>, HttpError> {
        crate::handlers::agents::agent_register(rqctx, body).await
    }

    async fn agent_register_status(
        rqctx: RequestContext<Self::Context>,
        query: Query<RegisterStatusQuery>,
    ) -> Result<HttpResponseOk<RegisterStatusResponse>, HttpError> {
        crate::handlers::agents::agent_register_status(rqctx, query).await
    }

    async fn list_cns(
        rqctx: RequestContext<Self::Context>,
        query: Query<CnListQuery>,
    ) -> Result<HttpResponseOk<Vec<CnView>>, HttpError> {
        crate::handlers::cns::list_cns(rqctx, query).await
    }

    async fn get_cn(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
    ) -> Result<HttpResponseOk<CnView>, HttpError> {
        crate::handlers::cns::get_cn(rqctx, path).await
    }

    async fn approve_cn(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<ApproveCnRequest>,
    ) -> Result<HttpResponseOk<CnView>, HttpError> {
        crate::handlers::cns::approve_cn(rqctx, body).await
    }

    async fn disable_cn(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
    ) -> Result<HttpResponseOk<CnView>, HttpError> {
        crate::handlers::cns::disable_cn(rqctx, path).await
    }

    async fn set_cn_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
        body: TypedBody<SetCnRoleRequest>,
    ) -> Result<HttpResponseOk<CnView>, HttpError> {
        crate::handlers::cns::set_cn_role(rqctx, path, body).await
    }

    async fn get_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Option<AutoApproveWindow>>, HttpError> {
        crate::handlers::cns::get_auto_approve_window(rqctx).await
    }

    async fn open_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<OpenAutoApproveRequest>,
    ) -> Result<HttpResponseOk<AutoApproveWindow>, HttpError> {
        crate::handlers::cns::open_auto_approve_window(rqctx, body).await
    }

    async fn close_auto_approve_window(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::cns::close_auto_approve_window(rqctx).await
    }

    async fn list_config(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<ConfigEntry>>, HttpError> {
        crate::handlers::config::list_config(rqctx).await
    }

    async fn get_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
        crate::handlers::config::get_config(rqctx, path).await
    }

    async fn set_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
        body: TypedBody<SetConfigRequest>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
        crate::handlers::config::set_config(rqctx, path, body).await
    }

    async fn reset_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ConfigKeyPath>,
    ) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
        crate::handlers::config::reset_config(rqctx, path).await
    }

    async fn list_legacy_cns(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<LegacyCnSummary>>, HttpError> {
        crate::handlers::legacy::list_legacy_cns(rqctx).await
    }

    async fn list_legacy_vms(
        rqctx: RequestContext<Self::Context>,
        query: Query<LegacyVmListQuery>,
    ) -> Result<HttpResponseOk<Vec<LegacyVm>>, HttpError> {
        crate::handlers::legacy::list_legacy_vms(rqctx, query).await
    }

    async fn get_legacy_vm(
        rqctx: RequestContext<Self::Context>,
        path: Path<LegacyVmPath>,
    ) -> Result<HttpResponseOk<LegacyVm>, HttpError> {
        crate::handlers::legacy::get_legacy_vm(rqctx, path).await
    }

    // ----- Storage clusters (operator-only) -----

    async fn list_storage_clusters(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<StorageClusterView>>, HttpError> {
        crate::handlers::storage_clusters::clusters::list_storage_clusters(rqctx).await
    }

    async fn create_storage_cluster(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewStorageCluster>,
    ) -> Result<HttpResponseCreated<StorageClusterView>, HttpError> {
        crate::handlers::storage_clusters::clusters::create_storage_cluster(rqctx, body).await
    }

    async fn get_storage_cluster(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<StorageClusterView>, HttpError> {
        crate::handlers::storage_clusters::clusters::get_storage_cluster(rqctx, path).await
    }

    async fn delete_storage_cluster(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::storage_clusters::clusters::delete_storage_cluster(rqctx, path).await
    }

    async fn probe_storage_cluster_health(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<StorageClusterView>, HttpError> {
        crate::handlers::storage_clusters::clusters::probe_storage_cluster_health(rqctx, path).await
    }

    async fn get_storage_cluster_summary(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<StorageClusterSummary>, HttpError> {
        crate::handlers::storage_clusters::clusters::get_storage_cluster_summary(rqctx, path).await
    }

    async fn list_storage_cluster_nodes(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<Vec<StorageNode>>, HttpError> {
        crate::handlers::storage_clusters::nodes::list_storage_cluster_nodes(rqctx, path).await
    }

    async fn get_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
    ) -> Result<HttpResponseOk<StorageNode>, HttpError> {
        crate::handlers::storage_clusters::nodes::get_storage_cluster_node(rqctx, path).await
    }

    async fn add_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<tritond_api::StorageAddNodeRequest>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
        crate::handlers::storage_clusters::nodes::add_storage_cluster_node(rqctx, path, body).await
    }

    async fn remove_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
        crate::handlers::storage_clusters::nodes::remove_storage_cluster_node(rqctx, path).await
    }

    async fn drain_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
        crate::handlers::storage_clusters::nodes::drain_storage_cluster_node(rqctx, path).await
    }

    async fn undrain_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
        crate::handlers::storage_clusters::nodes::undrain_storage_cluster_node(rqctx, path).await
    }

    async fn reweight_storage_cluster_node(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterNodePath>,
        body: TypedBody<tritond_api::StorageReweightRequest>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
        crate::handlers::storage_clusters::nodes::reweight_storage_cluster_node(rqctx, path, body)
            .await
    }

    async fn get_storage_cluster_membership(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<StorageMembership>, HttpError> {
        crate::handlers::storage_clusters::nodes::get_storage_cluster_membership(rqctx, path).await
    }

    async fn list_storage_cluster_buckets(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        query: Query<tritond_api::StorageBucketListQuery>,
    ) -> Result<HttpResponseOk<Vec<StorageBucket>>, HttpError> {
        crate::handlers::storage_clusters::buckets::list_storage_cluster_buckets(rqctx, path, query)
            .await
    }

    async fn get_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterBucketPath>,
    ) -> Result<HttpResponseOk<StorageBucket>, HttpError> {
        crate::handlers::storage_clusters::buckets::get_storage_cluster_bucket(rqctx, path).await
    }

    async fn create_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<tritond_api::StorageCreateBucketRequest>,
    ) -> Result<HttpResponseCreated<StorageBucket>, HttpError> {
        crate::handlers::storage_clusters::buckets::create_storage_cluster_bucket(rqctx, path, body)
            .await
    }

    async fn delete_storage_cluster_bucket(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterBucketPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::storage_clusters::buckets::delete_storage_cluster_bucket(rqctx, path).await
    }

    async fn list_storage_cluster_objects(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterBucketPath>,
        query: Query<tritond_api::StorageObjectsQuery>,
    ) -> Result<HttpResponseOk<StorageObjectsPage>, HttpError> {
        crate::handlers::storage_clusters::buckets::list_storage_cluster_objects(rqctx, path, query)
            .await
    }

    async fn list_storage_cluster_users(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
    ) -> Result<HttpResponseOk<Vec<StorageUser>>, HttpError> {
        crate::handlers::storage_clusters::users::list_storage_cluster_users(rqctx, path).await
    }

    async fn create_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<tritond_api::StorageCreateUserRequest>,
    ) -> Result<HttpResponseCreated<StorageUser>, HttpError> {
        crate::handlers::storage_clusters::users::create_storage_cluster_user(rqctx, path, body)
            .await
    }

    async fn get_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<StorageUser>, HttpError> {
        crate::handlers::storage_clusters::users::get_storage_cluster_user(rqctx, path).await
    }

    async fn delete_storage_cluster_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::storage_clusters::users::delete_storage_cluster_user(rqctx, path).await
    }

    async fn list_storage_cluster_access_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<Vec<StorageAccessKey>>, HttpError> {
        crate::handlers::storage_clusters::access_keys::list_storage_cluster_access_keys(
            rqctx, path,
        )
        .await
    }

    async fn create_storage_cluster_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseCreated<StorageAccessKey>, HttpError> {
        crate::handlers::storage_clusters::access_keys::create_storage_cluster_access_key(
            rqctx, path,
        )
        .await
    }

    async fn delete_storage_cluster_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterAccessKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::storage_clusters::access_keys::delete_storage_cluster_access_key(
            rqctx, path,
        )
        .await
    }

    async fn list_storage_cluster_user_policies(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPath>,
    ) -> Result<HttpResponseOk<Vec<String>>, HttpError> {
        crate::handlers::storage_clusters::policies::list_storage_cluster_user_policies(rqctx, path)
            .await
    }

    async fn get_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPolicyPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError> {
        crate::handlers::storage_clusters::policies::get_storage_cluster_user_policy(rqctx, path)
            .await
    }

    async fn put_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPolicyPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError> {
        crate::handlers::storage_clusters::policies::put_storage_cluster_user_policy(
            rqctx, path, body,
        )
        .await
    }

    async fn delete_storage_cluster_user_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterUserPolicyPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        crate::handlers::storage_clusters::policies::delete_storage_cluster_user_policy(rqctx, path)
            .await
    }

    async fn set_storage_cluster_presigner(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<SetPresignerRequest>,
    ) -> Result<HttpResponseOk<StorageClusterView>, HttpError> {
        crate::handlers::storage_clusters::presign::set_storage_cluster_presigner(rqctx, path, body)
            .await
    }

    async fn presign_storage_cluster_object_put(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<PresignPutRequest>,
    ) -> Result<HttpResponseOk<PresignResponse>, HttpError> {
        crate::handlers::storage_clusters::presign::presign_storage_cluster_object_put(
            rqctx, path, body,
        )
        .await
    }

    async fn presign_storage_cluster_object_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<StorageClusterPath>,
        body: TypedBody<PresignGetRequest>,
    ) -> Result<HttpResponseOk<PresignResponse>, HttpError> {
        crate::handlers::storage_clusters::presign::presign_storage_cluster_object_get(
            rqctx, path, body,
        )
        .await
    }

    // ----- Metrics -----

    async fn agent_metrics_ingest(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<tritond_metrics::SampleBatch>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError> {
        crate::handlers::telemetry::agent_metrics_ingest(rqctx, body).await
    }

    async fn instance_metrics_range(
        rqctx: RequestContext<Self::Context>,
        path: Path<TenantProjectInstancePath>,
        query: Query<MetricsRangeQuery>,
    ) -> Result<HttpResponseOk<tritond_metrics::RangeResult>, HttpError> {
        crate::handlers::telemetry::instance_metrics_range(rqctx, path, query).await
    }

    async fn cn_metrics_range(
        rqctx: RequestContext<Self::Context>,
        path: Path<CnPath>,
        query: Query<MetricsRangeQuery>,
    ) -> Result<HttpResponseOk<tritond_metrics::RangeResult>, HttpError> {
        crate::handlers::telemetry::cn_metrics_range(rqctx, path, query).await
    }

    // ----- Logs -----

    async fn agent_logs_ingest(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<tritond_logs::LogBatch>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError> {
        crate::handlers::telemetry::agent_logs_ingest(rqctx, body).await
    }

    async fn instance_logs_tail(
        rqctx: RequestContext<Self::Context>,
        path: Path<InstanceLogsPath>,
        query: Query<LogTailQuery>,
    ) -> Result<HttpResponseOk<tritond_logs::LogTailResult>, HttpError> {
        crate::handlers::telemetry::instance_logs_tail(rqctx, path, query).await
    }
}

/// Convert a short range identifier (`5m`, `1h`, `30d`) into the
/// corresponding `(since, until, step)` triple. Step values are sized
/// so each range yields ~60-100 buckets, which matches the SVG width
/// of the V5 dashboard's chart panels.
pub(crate) fn resolve_metrics_range(
    range: Option<&str>,
) -> Result<
    (
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
        chrono::Duration,
    ),
    HttpError,
> {
    let until = chrono::Utc::now();
    let (window, step) = match range.unwrap_or("1h") {
        "5m" => (chrono::Duration::minutes(5), chrono::Duration::seconds(5)),
        "15m" => (chrono::Duration::minutes(15), chrono::Duration::seconds(15)),
        "1h" => (chrono::Duration::hours(1), chrono::Duration::seconds(60)),
        "6h" => (chrono::Duration::hours(6), chrono::Duration::minutes(5)),
        "24h" => (chrono::Duration::hours(24), chrono::Duration::minutes(15)),
        "7d" => (chrono::Duration::days(7), chrono::Duration::hours(1)),
        "30d" => (chrono::Duration::days(30), chrono::Duration::hours(6)),
        other => {
            return Err(HttpError::for_bad_request(
                None,
                format!(
                    "unsupported range '{other}'; expected one of 5m, 15m, 1h, 6h, 24h, 7d, 30d"
                ),
            ));
        }
    };
    Ok((until - window, until, step))
}

/// Shared API-edge helper used by every per-scope
/// `create_ssh_key_*` handler: parse the openssh string,
/// compute the SHA-256 fingerprint, and on a parse failure
/// record a 400 audit event for the supplied principal +
/// extras blob and return the HTTP error to surface. On
/// success returns the canonical fingerprint.
pub(crate) async fn parse_and_audit_ssh_key(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    req: &NewSshKey,
    extras: serde_json::Value,
) -> Result<String, HttpError> {
    match parse_ssh_public_key(&req.public_key) {
        Ok(fp) => Ok(fp),
        Err(msg) => {
            ctx.audit
                .record_mutation(
                    principal,
                    Action::SshKeyCreate,
                    request_id,
                    None,
                    AuditOutcome::ClientError {
                        code: 400,
                        message: msg.clone(),
                    },
                    extras,
                )
                .await;
            Err(HttpError::for_bad_request(
                Some("BadRequest".to_string()),
                msg,
            ))
        }
    }
}

pub(crate) async fn audit_ssh_key_create_success(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    key: &SshKey,
    mut extras: serde_json::Value,
) {
    if let serde_json::Value::Object(ref mut map) = extras {
        map.insert("name".to_string(), serde_json::json!(key.name));
        map.insert(
            "fingerprint".to_string(),
            serde_json::json!(key.fingerprint),
        );
    }
    ctx.audit
        .record_mutation(
            principal,
            Action::SshKeyCreate,
            request_id,
            Some(format!("SshKey::\"{}\"", key.id)),
            AuditOutcome::Success {
                resource: Some(format!("SshKey::\"{}\"", key.id)),
            },
            extras,
        )
        .await;
}

pub(crate) async fn audit_ssh_key_create_failure(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    err: &StoreError,
) {
    ctx.audit
        .record_mutation(
            principal,
            Action::SshKeyCreate,
            request_id,
            None,
            store_error_to_audit_outcome(err),
            serde_json::Value::Null,
        )
        .await;
}

/// Shared sha256 / size_bytes API-edge validation used by every
/// per-scope `create_image_*` handler. On a validation failure,
/// records a 400 audit event for the supplied principal +
/// extras blob, and returns the HTTP error to surface. On
/// success returns `None` — the handler proceeds.
pub(crate) async fn validate_image_request(
    req: &NewImage,
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    extras: serde_json::Value,
) -> Option<HttpError> {
    if let Err(msg) = validate_sha256(&req.sha256) {
        ctx.audit
            .record_mutation(
                principal,
                Action::ImageCreate,
                request_id,
                None,
                AuditOutcome::ClientError {
                    code: 400,
                    message: msg.clone(),
                },
                extras,
            )
            .await;
        return Some(HttpError::for_bad_request(
            Some("BadRequest".to_string()),
            msg,
        ));
    }
    if req.size_bytes == 0 {
        let msg = "size_bytes must be greater than zero".to_string();
        ctx.audit
            .record_mutation(
                principal,
                Action::ImageCreate,
                request_id,
                None,
                AuditOutcome::ClientError {
                    code: 400,
                    message: msg.clone(),
                },
                extras,
            )
            .await;
        return Some(HttpError::for_bad_request(
            Some("BadRequest".to_string()),
            msg,
        ));
    }
    None
}

pub(crate) async fn audit_image_create_success(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    image: &Image,
    mut extras: serde_json::Value,
) {
    if let serde_json::Value::Object(ref mut map) = extras {
        map.insert("name".to_string(), serde_json::json!(image.name));
        map.insert("sha256".to_string(), serde_json::json!(image.sha256));
    }
    ctx.audit
        .record_mutation(
            principal,
            Action::ImageCreate,
            request_id,
            Some(format!("Image::\"{}\"", image.id)),
            AuditOutcome::Success {
                resource: Some(format!("Image::\"{}\"", image.id)),
            },
            extras,
        )
        .await;
}

pub(crate) async fn audit_image_create_failure(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    err: &StoreError,
) {
    ctx.audit
        .record_mutation(
            principal,
            Action::ImageCreate,
            request_id,
            None,
            store_error_to_audit_outcome(err),
            serde_json::Value::Null,
        )
        .await;
}

pub(crate) fn mint_token_pair(
    auth: &AuthService,
    user_id: Uuid,
) -> Result<TokenResponse, HttpError> {
    let (access_token, access_expires_at) = mint_access(auth.jwt_key(), user_id)
        .map_err(|e| HttpError::for_internal_error(format!("mint access: {e}")))?;
    let (refresh_token, refresh_expires_at) = mint_refresh(auth.jwt_key(), user_id)
        .map_err(|e| HttpError::for_internal_error(format!("mint refresh: {e}")))?;
    Ok(TokenResponse {
        access_token,
        refresh_token,
        access_expires_at,
        refresh_expires_at,
    })
}

/// Parse a `/v2/config/{key}` path segment into a [`ConfigKey`], or
/// `404` for an unrecognised name.
pub(crate) fn config_key_or_404(raw: &str) -> Result<ConfigKey, HttpError> {
    ConfigKey::from_wire(raw).ok_or_else(|| {
        HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            format!("unknown config key: {raw}"),
        )
    })
}

/// Build the wire view of one config key against a `Settings` snapshot,
/// flagging any legacy env var currently shadowing it at boot.
pub(crate) fn build_config_entry(
    key: ConfigKey,
    settings: &tritond_store::Settings,
) -> ConfigEntry {
    ConfigEntry {
        key: key.as_str().to_string(),
        value: settings.get(key),
        default: tritond_store::Settings::default().get(key),
        env_override: crate::settings::env_override_for(key).map(str::to_string),
        restart_required: key.restart_required(),
        description: key.description().to_string(),
    }
}

/// Build the [`ApiDescription`] for `tritond`.
pub fn api_description() -> Result<ApiDescription<ApiContext>> {
    tritond_api::tritond_api_mod::api_description::<TritondServiceImpl>()
        .map_err(|e| anyhow::anyhow!("failed to build API description: {e}"))
}

/// Start a Dropshot server with a freshly-constructed in-memory store
/// and a fresh random JWT key. Convenience wrapper for tests and
/// `main` paths that don't need bootstrap-from-store semantics.
pub async fn start_server(bind_address: &str) -> Result<HttpServer<ApiContext>> {
    let context = ApiContext::in_memory().context("build in-memory api context")?;
    start_server_with_context(bind_address, context).await
}

/// Start a Dropshot server backed by an externally-built [`ApiContext`].
///
/// Also spawns the in-process stub provisioner (see
/// [`crate::provisioner`]) so any provisioning jobs the API
/// handlers enqueue get processed. The provisioner runs as a
/// detached tokio task and exits when the runtime shuts down. A
/// future deploy with a real per-CN `tritonagent` will skip the
/// stub spawn (gated by config).
pub async fn start_server_with_context(
    bind_address: &str,
    context: ApiContext,
) -> Result<HttpServer<ApiContext>> {
    let parsed: SocketAddr = bind_address
        .parse()
        .with_context(|| format!("invalid bind address: {bind_address}"))?;

    let config_dropshot = ConfigDropshot {
        bind_address: parsed,
        // The default 1 KB body cap is too small for `/v2/agent/register`,
        // which carries the full SmartOS `sysinfo` JSON (tens of KB on a
        // production CN). 1 MB is plenty for any expected payload and
        // still bounds an abusive client.
        default_request_body_max_bytes: 1_048_576,
        ..Default::default()
    };

    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    }
    .to_logger("tritond")
    .map_err(|e| anyhow::anyhow!("failed to construct logger: {e}"))?;

    let api = api_description()?;
    // Spawn the stub provisioner before starting the HTTP server
    // so the queue is being drained from the moment handlers can
    // accept requests. Tests / real-agent deploys can opt out via
    // `ApiContext::without_in_process_provisioner`.
    if context.spawn_in_process_provisioner {
        let _provisioner = provisioner::spawn(Arc::clone(&context.store));
    }

    // The sweeper runs alongside the in-process stub or a real
    // agent — its job is to reap claims that *no* worker
    // completed (agent crash, partition). Configurable per
    // [`ApiContext::with_sweeper`]; tests typically leave it
    // off for deterministic state.
    if let Some(sw) = context.sweeper {
        let _sweeper = sweeper::spawn(
            Arc::clone(&context.store),
            Arc::clone(&context.audit),
            sw.interval,
            sw.stale_after,
        );
    }

    // The DHCP-lease reconciler (γ.3) walks list_all_dhcp_leases
    // periodically and reaps orphaned, unpinned, stale leases. See
    // dhcp_reconciler module docs for the exact GC criteria.
    // Configurable per [`ApiContext::with_dhcp_reconciler`]; tests
    // typically leave it off so explicit IPAM-state assertions
    // aren't raced.
    if let Some(rc) = context.dhcp_reconciler {
        let _reconciler =
            dhcp_reconciler::spawn(Arc::clone(&context.store), Arc::clone(&context.audit), rc);
    }

    let server = HttpServerStarter::new(&config_dropshot, api, context, &log)
        .map_err(|e| anyhow::anyhow!("failed to start HTTP server: {e}"))?
        .start();

    Ok(server)
}
