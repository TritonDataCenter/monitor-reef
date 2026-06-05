// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! NAPI Client Library
//!
//! This client provides typed access to the Triton NAPI service
//! (Networking API).
//!
//! ## Usage
//!
//! ```ignore
//! use napi_client::Client;
//!
//! let client = Client::new("http://napi.my-dc.my-cloud.local");
//!
//! // List NICs
//! let nics = client.list_nics().send().await?;
//!
//! // Get a specific NIC by MAC
//! let nic = client.get_nic().mac("90:b8:d0:a5:e3:01").send().await?;
//!
//! // List networks
//! let networks = client.list_networks().send().await?;
//!
//! // Create a NIC tag
//! use napi_client::CreateNicTagBody;
//! let body = CreateNicTagBody {
//!     name: "external".to_string(),
//!     uuid: None,
//!     mtu: None,
//! };
//! client.create_nic_tag().body(body).send().await?;
//! ```

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

// Re-export types from the API crate for convenience.
pub use napi_api::{
    // Aggregation types
    Aggregation,
    AggregationPath,
    BelongsToType,
    CreateAggregationBody,
    // Common types
    // Fabric types
    CreateFabricNetworkBody,
    CreateFabricVlanBody,
    // Network types
    CreateNetworkBody,
    CreateNetworkNicBody,
    // Pool types
    CreateNetworkPoolBody,
    // NIC types
    CreateNicBody,
    // NIC tag types
    CreateNicTagBody,
    // Fabric paths
    FabricNetworkPath,
    FabricOwnerPath,
    FabricVlan,
    FabricVlanPath,
    // Manage types
    GcResponse,
    // IP types
    Ip,
    IpPath,
    LacpMode,
    ListAggregationsQuery,
    ListFabricNetworksQuery,
    ListFabricVlansQuery,
    ListIpsQuery,
    ListNetworkPoolsQuery,
    ListNetworksQuery,
    ListNicTagsQuery,
    ListNicsQuery,
    MemoryUsage,
    MorayServiceStatus,
    Network,
    NetworkFamily,
    NetworkPath,
    NetworkPool,
    NetworkPoolPath,
    NetworkSubPath,
    Nic,
    NicPath,
    NicState,
    NicTag,
    NicTagPath,
    // Ping types
    PingConfig,
    PingResponse,
    PingServices,
    PingStatus,
    // Search types
    SearchIpsQuery,
    UpdateAggregationBody,
    UpdateFabricNetworkBody,
    UpdateFabricVlanBody,
    UpdateIpBody,
    UpdateNetworkBody,
    UpdateNetworkPoolBody,
    UpdateNicBody,
    UpdateNicTagBody,
    Uuid,
};
