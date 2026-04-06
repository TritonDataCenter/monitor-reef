// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! NAPI (Networking API) trait definition
//!
//! This crate defines the API trait for Triton's NAPI service (version 1.4.3).
//! NAPI is an internal HTTP API for managing networking resources in a Triton
//! datacenter: NICs, NIC tags, networks, network pools, IPs, aggregations,
//! fabric VLANs, and fabric networks.
//!
//! # JSON Field Naming
//!
//! NAPI uses snake_case for all JSON field names, which is standard for Triton
//! internal APIs. The only exceptions are `heapTotal` and `heapUsed` in the
//! GcResponse, which come from Node.js `process.memoryUsage()`.

use dropshot::{
    HttpError, HttpResponseOk, HttpResponseUpdatedNoContent, Path, Query, RequestContext, TypedBody,
};

pub mod types;
pub use types::*;

/// NAPI trait definition
///
/// This trait defines all endpoints of the Triton NAPI service (version 1.4.3).
/// The API is organized into the following categories:
/// - Ping (health check)
/// - NICs (network interface cards)
/// - NIC Tags (NIC tag management)
/// - Networks (network CRUD)
/// - Network Pools (network pool CRUD)
/// - Network IPs (IP address management within networks)
/// - Network NICs (provision NICs on specific networks)
/// - Aggregations (link aggregation / bonding)
/// - Fabric VLANs (fabric overlay VLANs)
/// - Fabric Networks (networks within fabric VLANs)
/// - Search (cross-network IP search)
/// - Manage (garbage collection)
#[dropshot::api_description]
pub trait NapiApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    // ========================================================================
    // Ping Endpoint
    // ========================================================================

    /// Health check endpoint
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["ping"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    // ========================================================================
    // NIC Endpoints
    // ========================================================================

    /// List NICs
    ///
    /// Returns an array of NIC objects matching the query filters.
    #[endpoint {
        method = GET,
        path = "/nics",
        tags = ["nics"],
    }]
    async fn list_nics(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListNicsQuery>,
    ) -> Result<HttpResponseOk<Vec<Nic>>, HttpError>;

    /// Create a NIC
    ///
    /// Provisions a new NIC. Returns 200 (not 201) on success.
    #[endpoint {
        method = POST,
        path = "/nics",
        tags = ["nics"],
    }]
    async fn create_nic(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateNicBody>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    /// Get a NIC by MAC address
    ///
    /// The MAC address can be colon-separated, dash-separated, or bare hex.
    #[endpoint {
        method = GET,
        path = "/nics/{mac}",
        tags = ["nics"],
    }]
    async fn get_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    /// Update a NIC
    #[endpoint {
        method = PUT,
        path = "/nics/{mac}",
        tags = ["nics"],
    }]
    async fn update_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicPath>,
        body: TypedBody<UpdateNicBody>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    /// Delete a NIC
    #[endpoint {
        method = DELETE,
        path = "/nics/{mac}",
        tags = ["nics"],
    }]
    async fn delete_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // NIC Tag Endpoints
    // ========================================================================

    /// List NIC tags
    #[endpoint {
        method = GET,
        path = "/nic_tags",
        tags = ["nic_tags"],
    }]
    async fn list_nic_tags(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListNicTagsQuery>,
    ) -> Result<HttpResponseOk<Vec<NicTag>>, HttpError>;

    /// Create a NIC tag
    ///
    /// Returns 200 (not 201) on success.
    #[endpoint {
        method = POST,
        path = "/nic_tags",
        tags = ["nic_tags"],
    }]
    async fn create_nic_tag(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateNicTagBody>,
    ) -> Result<HttpResponseOk<NicTag>, HttpError>;

    /// Get a NIC tag by name
    #[endpoint {
        method = GET,
        path = "/nic_tags/{name}",
        tags = ["nic_tags"],
    }]
    async fn get_nic_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicTagPath>,
    ) -> Result<HttpResponseOk<NicTag>, HttpError>;

    /// Rename/update a NIC tag
    ///
    /// The path parameter is the current (old) name of the NIC tag.
    #[endpoint {
        method = PUT,
        path = "/nic_tags/{name}",
        tags = ["nic_tags"],
    }]
    async fn update_nic_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicTagPath>,
        body: TypedBody<UpdateNicTagBody>,
    ) -> Result<HttpResponseOk<NicTag>, HttpError>;

    /// Delete a NIC tag
    #[endpoint {
        method = DELETE,
        path = "/nic_tags/{name}",
        tags = ["nic_tags"],
    }]
    async fn delete_nic_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicTagPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // Network Endpoints
    // ========================================================================

    /// List networks
    #[endpoint {
        method = GET,
        path = "/networks",
        tags = ["networks"],
    }]
    async fn list_networks(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListNetworksQuery>,
    ) -> Result<HttpResponseOk<Vec<Network>>, HttpError>;

    /// Create a network
    ///
    /// Returns 200 (not 201) on success.
    #[endpoint {
        method = POST,
        path = "/networks",
        tags = ["networks"],
    }]
    async fn create_network(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateNetworkBody>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Get a network
    ///
    /// The UUID parameter can also be the literal string "admin" to look up
    /// the admin network.
    #[endpoint {
        method = GET,
        path = "/networks/{uuid}",
        tags = ["networks"],
    }]
    async fn get_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPath>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Update a network
    #[endpoint {
        method = PUT,
        path = "/networks/{uuid}",
        tags = ["networks"],
    }]
    async fn update_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPath>,
        body: TypedBody<UpdateNetworkBody>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Delete a network
    ///
    /// Fails if the network is in a pool.
    #[endpoint {
        method = DELETE,
        path = "/networks/{uuid}",
        tags = ["networks"],
    }]
    async fn delete_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // Network Pool Endpoints
    // ========================================================================

    /// List network pools
    #[endpoint {
        method = GET,
        path = "/network_pools",
        tags = ["network_pools"],
    }]
    async fn list_network_pools(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListNetworkPoolsQuery>,
    ) -> Result<HttpResponseOk<Vec<NetworkPool>>, HttpError>;

    /// Create a network pool
    ///
    /// Returns 200 (not 201) on success.
    #[endpoint {
        method = POST,
        path = "/network_pools",
        tags = ["network_pools"],
    }]
    async fn create_network_pool(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateNetworkPoolBody>,
    ) -> Result<HttpResponseOk<NetworkPool>, HttpError>;

    /// Get a network pool
    #[endpoint {
        method = GET,
        path = "/network_pools/{uuid}",
        tags = ["network_pools"],
    }]
    async fn get_network_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPoolPath>,
    ) -> Result<HttpResponseOk<NetworkPool>, HttpError>;

    /// Update a network pool
    #[endpoint {
        method = PUT,
        path = "/network_pools/{uuid}",
        tags = ["network_pools"],
    }]
    async fn update_network_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPoolPath>,
        body: TypedBody<UpdateNetworkPoolBody>,
    ) -> Result<HttpResponseOk<NetworkPool>, HttpError>;

    /// Delete a network pool
    #[endpoint {
        method = DELETE,
        path = "/network_pools/{uuid}",
        tags = ["network_pools"],
    }]
    async fn delete_network_pool(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPoolPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // Network IP Endpoints
    // ========================================================================

    /// List IPs in a network
    #[endpoint {
        method = GET,
        path = "/networks/{uuid}/ips",
        tags = ["ips"],
    }]
    async fn list_ips(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkSubPath>,
        query: Query<ListIpsQuery>,
    ) -> Result<HttpResponseOk<Vec<Ip>>, HttpError>;

    /// Get an IP in a network
    ///
    /// Returns an IP object even if the IP is not stored in Moray
    /// (synthesizes a "free" IP record).
    #[endpoint {
        method = GET,
        path = "/networks/{uuid}/ips/{ip_addr}",
        tags = ["ips"],
    }]
    async fn get_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<IpPath>,
    ) -> Result<HttpResponseOk<Ip>, HttpError>;

    /// Update (or free) an IP in a network
    ///
    /// Creates or updates an IP record. Setting `free=true` deletes the
    /// assignment instead.
    #[endpoint {
        method = PUT,
        path = "/networks/{uuid}/ips/{ip_addr}",
        tags = ["ips"],
    }]
    async fn update_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<IpPath>,
        body: TypedBody<UpdateIpBody>,
    ) -> Result<HttpResponseOk<Ip>, HttpError>;

    // ========================================================================
    // Network NIC Endpoints
    // ========================================================================

    /// Provision a NIC on a specific network
    ///
    /// Returns 200 (not 201) on success.
    #[endpoint {
        method = POST,
        path = "/networks/{uuid}/nics",
        tags = ["nics"],
    }]
    async fn create_network_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkSubPath>,
        body: TypedBody<CreateNetworkNicBody>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    // ========================================================================
    // Aggregation Endpoints
    // ========================================================================

    /// List aggregations
    #[endpoint {
        method = GET,
        path = "/aggregations",
        tags = ["aggregations"],
    }]
    async fn list_aggregations(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListAggregationsQuery>,
    ) -> Result<HttpResponseOk<Vec<Aggregation>>, HttpError>;

    /// Create an aggregation
    ///
    /// Returns 200 (not 201) on success.
    #[endpoint {
        method = POST,
        path = "/aggregations",
        tags = ["aggregations"],
    }]
    async fn create_aggregation(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateAggregationBody>,
    ) -> Result<HttpResponseOk<Aggregation>, HttpError>;

    /// Get an aggregation
    #[endpoint {
        method = GET,
        path = "/aggregations/{id}",
        tags = ["aggregations"],
    }]
    async fn get_aggregation(
        rqctx: RequestContext<Self::Context>,
        path: Path<AggregationPath>,
    ) -> Result<HttpResponseOk<Aggregation>, HttpError>;

    /// Update an aggregation
    #[endpoint {
        method = PUT,
        path = "/aggregations/{id}",
        tags = ["aggregations"],
    }]
    async fn update_aggregation(
        rqctx: RequestContext<Self::Context>,
        path: Path<AggregationPath>,
        body: TypedBody<UpdateAggregationBody>,
    ) -> Result<HttpResponseOk<Aggregation>, HttpError>;

    /// Delete an aggregation
    #[endpoint {
        method = DELETE,
        path = "/aggregations/{id}",
        tags = ["aggregations"],
    }]
    async fn delete_aggregation(
        rqctx: RequestContext<Self::Context>,
        path: Path<AggregationPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // Search Endpoints
    // ========================================================================

    /// Search for an IP across all networks
    ///
    /// Returns 404 if no matching IP is found.
    #[endpoint {
        method = GET,
        path = "/search/ips",
        tags = ["search"],
    }]
    async fn search_ips(
        rqctx: RequestContext<Self::Context>,
        query: Query<SearchIpsQuery>,
    ) -> Result<HttpResponseOk<Vec<Ip>>, HttpError>;

    // ========================================================================
    // Manage Endpoints
    // ========================================================================

    /// Run garbage collection
    ///
    /// Returns 501 NotImplemented if GC is not exposed.
    #[endpoint {
        method = GET,
        path = "/manage/gc",
        tags = ["manage"],
    }]
    async fn run_gc(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<GcResponse>, HttpError>;

    // ========================================================================
    // Fabric VLAN Endpoints
    // ========================================================================

    /// List fabric VLANs for an owner
    ///
    /// Requires overlays (fabrics) to be enabled.
    #[endpoint {
        method = GET,
        path = "/fabrics/{owner_uuid}/vlans",
        tags = ["fabric_vlans"],
    }]
    async fn list_fabric_vlans(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricOwnerPath>,
        query: Query<ListFabricVlansQuery>,
    ) -> Result<HttpResponseOk<Vec<FabricVlan>>, HttpError>;

    /// Create a fabric VLAN
    ///
    /// Returns 200 (not 201) on success. Requires overlays to be enabled.
    #[endpoint {
        method = POST,
        path = "/fabrics/{owner_uuid}/vlans",
        tags = ["fabric_vlans"],
    }]
    async fn create_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricOwnerPath>,
        body: TypedBody<CreateFabricVlanBody>,
    ) -> Result<HttpResponseOk<FabricVlan>, HttpError>;

    /// Get a fabric VLAN
    #[endpoint {
        method = GET,
        path = "/fabrics/{owner_uuid}/vlans/{vlan_id}",
        tags = ["fabric_vlans"],
    }]
    async fn get_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
    ) -> Result<HttpResponseOk<FabricVlan>, HttpError>;

    /// Update a fabric VLAN
    #[endpoint {
        method = PUT,
        path = "/fabrics/{owner_uuid}/vlans/{vlan_id}",
        tags = ["fabric_vlans"],
    }]
    async fn update_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
        body: TypedBody<UpdateFabricVlanBody>,
    ) -> Result<HttpResponseOk<FabricVlan>, HttpError>;

    /// Delete a fabric VLAN
    ///
    /// Fails if there are networks on this VLAN.
    #[endpoint {
        method = DELETE,
        path = "/fabrics/{owner_uuid}/vlans/{vlan_id}",
        tags = ["fabric_vlans"],
    }]
    async fn delete_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // Fabric Network Endpoints
    // ========================================================================

    /// List fabric networks on a VLAN
    #[endpoint {
        method = GET,
        path = "/fabrics/{owner_uuid}/vlans/{vlan_id}/networks",
        tags = ["fabric_networks"],
    }]
    async fn list_fabric_networks(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
        query: Query<ListFabricNetworksQuery>,
    ) -> Result<HttpResponseOk<Vec<Network>>, HttpError>;

    /// Create a fabric network on a VLAN
    ///
    /// MTU, nic_tag, and vnet_id are auto-set from the VLAN.
    /// Returns 200 (not 201) on success.
    #[endpoint {
        method = POST,
        path = "/fabrics/{owner_uuid}/vlans/{vlan_id}/networks",
        tags = ["fabric_networks"],
    }]
    async fn create_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
        body: TypedBody<CreateFabricNetworkBody>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Get a fabric network
    #[endpoint {
        method = GET,
        path = "/fabrics/{owner_uuid}/vlans/{vlan_id}/networks/{uuid}",
        tags = ["fabric_networks"],
    }]
    async fn get_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricNetworkPath>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Update a fabric network
    #[endpoint {
        method = PUT,
        path = "/fabrics/{owner_uuid}/vlans/{vlan_id}/networks/{uuid}",
        tags = ["fabric_networks"],
    }]
    async fn update_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricNetworkPath>,
        body: TypedBody<UpdateFabricNetworkBody>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Delete a fabric network
    #[endpoint {
        method = DELETE,
        path = "/fabrics/{owner_uuid}/vlans/{vlan_id}/networks/{uuid}",
        tags = ["fabric_networks"],
    }]
    async fn delete_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricNetworkPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;
}
