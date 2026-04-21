// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Gateway Client Library
//!
//! Progenitor-generated client for the merged `triton-gateway-api.json`
//! OpenAPI spec. Covers both the tritonapi-native `/v1/*` surface (login,
//! refresh, session, JWKS) and the cloudapi surface proxied through the
//! gateway (`/{account}/*`).
//!
//! ## Auth model
//!
//! Two pluggable styles, selected at client-construction time:
//!
//! - **Bearer JWT** (primary): pair the client with a
//!   [`auth::TokenProvider`] impl; every request is stamped with
//!   `Authorization: Bearer <jwt>`.
//! - **SSH HTTP Signature** (fallback, useful for dev / parity testing):
//!   delegates to [`triton_auth::sign_request`] exactly like
//!   `cloudapi-client` does.
//!
//! The selection lives in [`auth::GatewayAuthMethod`]; per-client it is
//! packaged alongside `Accept-Version` / `X-Act-As` into a
//! [`GatewayAuthConfig`].
//!
//! ## Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use triton_gateway_client::{TypedClient, GatewayAuthConfig, auth::TokenProvider};
//!
//! # async fn demo(my_provider: Arc<dyn TokenProvider>) -> anyhow::Result<()> {
//! let cfg = GatewayAuthConfig::bearer(my_provider);
//! let client = TypedClient::new("https://gateway.example.com", cfg);
//!
//! let machines = client
//!     .inner()
//!     .list_machines()
//!     .account("myaccount")
//!     .send()
//!     .await?
//!     .into_inner();
//! # Ok(()) }
//! ```

pub mod auth;

// Allow unwrap in generated code — Progenitor uses it in Client::new().
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

// Re-export auth primitives for call-site ergonomics.
pub use auth::{GatewayAuthConfig, GatewayAuthMethod, TokenProvider, add_auth_headers};

// Re-export triton-auth types for the SSH fallback path.
pub use triton_auth::{AuthConfig, AuthError, KeySource};

// =============================================================================
// API-type re-exports (canonical API-crate types, per CLAUDE.md §6).
//
// Both triton-api and cloudapi-api export through `pub use types::*`; we
// surface the same names from both here so CLI code can import one flat
// namespace. The two crates were audited for collisions at Phase 2 time:
// - `Uuid` is identical in both (both alias `uuid::Uuid` via vmapi_api).
// - cloudapi-api exports `ErrorResponse` (not `Error`); triton-api does
//   not export an `Error`; the merged OpenAPI spec uses tritonapi's error
//   body shape (per Phase 0 + Phase 1 decisions). No runtime collision.
// - No other type names overlap between the two crates as of this writing.
// =============================================================================

// Tritonapi-native types (login, refresh, session, JWKS, ping).
pub use triton_api::{
    Jwk, JwkSet, LoginRequest, LoginResponse, LogoutResponse, PingResponse, RefreshRequest,
    RefreshResponse, SessionResponse, UserInfo,
};

// CloudAPI types (same set cloudapi-client re-exports; keep the two lists
// aligned so shared command handlers get identical type names regardless of
// which client they target).
pub use cloudapi_api::{
    AccessKey, AccessKeyPath, AccessKeyStatus, Account, AccountPath, AddForeignDatacenterRequest,
    AddMetadataRequest, AddNicRequest, AuditEntry, ChangePasswordRequest, ChangefeedChangeKind,
    ChangefeedMessage, ChangefeedResource, ChangefeedSubResource, ChangefeedSubscription,
    CloneImageRequest, Config, CreateAccessKeyRequest, CreateAccessKeyResponse, CreateDiskRequest,
    CreateFabricNetworkRequest, CreateFabricVlanRequest, CreateFirewallRuleRequest,
    CreateImageRequest, CreateMachineRequest, CreatePolicyRequest, CreateRoleRequest,
    CreateSnapshotRequest, CreateSshKeyRequest, CreateUserRequest, CreateVolumeRequest,
    CredentialType, Datacenter, Datacenters, DisableDeletionProtectionRequest,
    DisableFirewallRequest, Disk, DiskAction, DiskActionQuery, DiskPath, DiskSpec, DiskState,
    EnableDeletionProtectionRequest, EnableFirewallRequest, ExportImageRequest, FabricNetworkPath,
    FabricVlan, FabricVlanPath, FirewallRule, FirewallRulePath, Image, ImageAction,
    ImageActionQuery, ImageCollectionActionQuery, ImagePath, ImageState, ImageType,
    ImportImageRequest, KeyPath, ListImagesQuery, ListMachinesQuery, Machine, MachineAction,
    MachineActionQuery, MachineNic, MachinePath, MachineState, Metadata, MetadataKeyPath,
    MigrateRequest, Migration, MigrationAction, MigrationEstimate, MigrationEstimateRequest,
    MigrationPhase, MigrationState, MountMode, Network, NetworkIp, NetworkIpPath, NetworkObject,
    NetworkPath, Nic, NicPath, NicState, Package, PackagePath, Policy, PolicyPath,
    ProvisioningLimits, RebootMachineRequest, RenameMachineRequest, ReplaceRoleTagsRequest,
    ResizeDiskRequest, ResizeMachineRequest, Role, RolePath, Service, Services, Snapshot,
    SnapshotPath, SnapshotState, SshKey, StartMachineRequest, StopMachineRequest, TagPath, Tags,
    TagsRequest, Timestamp, UpdateAccessKeyRequest, UpdateAccountRequest, UpdateConfigRequest,
    UpdateFabricNetworkRequest, UpdateFabricVlanRequest, UpdateFirewallRuleRequest,
    UpdateImageRequest, UpdateNetworkIpRequest, UpdatePolicyRequest, UpdateRoleRequest,
    UpdateUserRequest, UpdateVolumeRequest, User, UserAccessKeyPath, UserPath, Uuid, VmState,
    Volume, VolumeAction, VolumeActionQuery, VolumeMount, VolumePath, VolumeSize,
};

// =============================================================================
// Client wrappers
//
// Parallel to cloudapi-client's `AuthenticatedClient` / `TypedClient`. The
// generated `Client` is fully functional on its own; these wrappers are
// ergonomic sugar for constructing a client with a pre-built reqwest client
// and keeping a handle on the auth config alongside it.
// =============================================================================

/// Authenticated gateway client wrapper.
///
/// Owns a Progenitor-generated [`Client`] configured with a [`GatewayAuthConfig`]
/// so every outgoing request is stamped with the appropriate auth headers via
/// the `pre_hook_async` in [`auth::add_auth_headers`].
pub struct AuthenticatedClient {
    inner: Client,
    auth_config: GatewayAuthConfig,
}

impl AuthenticatedClient {
    /// Create a new authenticated gateway client.
    pub fn new(base_url: &str, auth_config: GatewayAuthConfig) -> Self {
        Self {
            inner: Client::new_with_client(base_url, reqwest::Client::new(), auth_config.clone()),
            auth_config,
        }
    }

    /// Access the underlying Progenitor client.
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    /// Get the current auth configuration.
    pub fn auth_config(&self) -> &GatewayAuthConfig {
        &self.auth_config
    }
}

/// Typed wrapper matching the shape of `cloudapi-client::TypedClient` so
/// callers can swap clients (e.g., dispatch from a `TritonClient` trait in a
/// CLI) with minimal friction.
///
/// Phase 2 exposes the bare minimum: construction + access to the inner
/// client + the underlying reqwest HTTP client. Method-wise wrappers
/// (action-dispatch helpers, share/unshare multi-step logic, etc.) will be
/// added in Phase 4 when the CLI actually routes through this client.
pub struct TypedClient {
    inner: Client,
    auth_config: GatewayAuthConfig,
    http_client: reqwest::Client,
}

impl TypedClient {
    /// Create a new typed client wrapper.
    pub fn new(base_url: &str, auth_config: GatewayAuthConfig) -> Self {
        let http_client = reqwest::Client::new();
        Self {
            inner: Client::new_with_client(base_url, http_client.clone(), auth_config.clone()),
            auth_config,
            http_client,
        }
    }

    /// Create a client with an optional insecure-TLS flag.
    ///
    /// # Errors
    /// Returns an error if the underlying reqwest client cannot be built.
    pub fn new_with_insecure(
        base_url: &str,
        auth_config: GatewayAuthConfig,
        insecure: bool,
    ) -> Result<Self, reqwest::Error> {
        let http_client = reqwest::Client::builder()
            .danger_accept_invalid_certs(insecure)
            .build()?;
        Ok(Self {
            inner: Client::new_with_client(base_url, http_client.clone(), auth_config.clone()),
            auth_config,
            http_client,
        })
    }

    /// Create a client reusing a caller-supplied reqwest client (lets the
    /// caller control TLS / root-store / proxies).
    pub fn new_with_http_client(
        base_url: &str,
        auth_config: GatewayAuthConfig,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            inner: Client::new_with_client(base_url, http_client.clone(), auth_config.clone()),
            auth_config,
            http_client,
        }
    }

    /// Access the underlying Progenitor client.
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    /// Get the current auth configuration.
    pub fn auth_config(&self) -> &GatewayAuthConfig {
        &self.auth_config
    }

    /// Access the underlying reqwest HTTP client (useful for non-progenitor
    /// operations that should reuse the same TLS / connection pool).
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Base URL this client is connected to.
    pub fn baseurl(&self) -> &str {
        use progenitor_client::ClientInfo;
        self.inner.baseurl()
    }
}
