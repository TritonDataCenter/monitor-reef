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

/// Re-export of the shared limit/offset pagination helper.
///
/// The helper itself lives in `triton-pagination` so both this crate and
/// `cloudapi-client` can share a single implementation. Gateway-route
/// list commands in the CLI reach for `triton_gateway_client::pagination`
/// through this alias.
pub use triton_pagination as pagination;

// Allow unwrap in generated code — Progenitor uses it in Client::new().
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

#[cfg(debug_assertions)]
use std::sync::OnceLock;
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, Ordering};

use progenitor_client::{ClientHooks, OperationInfo};

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

// Action-dispatch request structs come from the Progenitor-generated
// `types` module because openapi-manager injects their schemas into
// components.schemas (see docs/design/action-dispatch-openapi.md). Mirror
// cloudapi-client's split here so consumers see identical import paths
// regardless of which client they reach for.
pub use types::{
    CloneImageRequest, DisableDeletionProtectionRequest, DisableFirewallRequest,
    EnableDeletionProtectionRequest, EnableFirewallRequest, ExportImageRequest, ImportImageRequest,
    RebootMachineRequest, RenameMachineRequest, ResizeDiskRequest, ResizeMachineRequest,
    StartMachineRequest, StopMachineRequest, UpdateImageRequest, UpdateVolumeRequest,
};

// CloudAPI types (same set cloudapi-client re-exports; keep the two lists
// aligned so shared command handlers get identical type names regardless of
// which client they target).
pub use cloudapi_api::{
    AccessKey, AccessKeyPath, AccessKeyStatus, Account, AccountPath, AddForeignDatacenterRequest,
    AddMetadataRequest, AddNicRequest, AffinityRules, AuditEntry, ChangePasswordRequest,
    ChangefeedChangeKind, ChangefeedMessage, ChangefeedResource, ChangefeedSubResource,
    ChangefeedSubscription, Config, CreateAccessKeyRequest, CreateAccessKeyResponse,
    CreateDiskRequest, CreateFabricNetworkRequest, CreateFabricVlanRequest,
    CreateFirewallRuleRequest, CreateImageRequest, CreateMachineRequest, CreatePolicyRequest,
    CreateRoleRequest, CreateSnapshotRequest, CreateSshKeyRequest, CreateUserRequest,
    CreateVolumeRequest, CredentialType, Datacenter, Datacenters, Disk, DiskAction,
    DiskActionQuery, DiskPath, DiskSpec, DiskState, FabricNetworkPath, FabricVlan, FabricVlanPath,
    FirewallRule, FirewallRulePath, Image, ImageAcl, ImageAction, ImageActionQuery,
    ImageCollectionActionQuery, ImagePath, ImageState, ImageType, KeyPath, ListImagesQuery,
    ListMachinesQuery, Machine, MachineAction, MachineActionQuery, MachineNic, MachinePath,
    MachineState, Metadata, MetadataKeyPath, MigrateRequest, Migration, MigrationAction,
    MigrationEstimate, MigrationEstimateRequest, MigrationPhase, MigrationState, MountMode,
    Network, NetworkIds, NetworkIp, NetworkIpPath, NetworkObject, NetworkPath, Nic, NicPath,
    NicState, Package, PackagePath, Policy, PolicyPath, PolicyRef, PolicyRules, ProvisioningLimit,
    ProvisioningLimits, ReplaceRoleTagsRequest, Resolvers, Role, RolePath, RoleTags, Service,
    Services, Snapshot, SnapshotPath, SnapshotState, SshKey, TagPath, Tags, TagsRequest, Timestamp,
    UpdateAccessKeyRequest, UpdateAccountRequest, UpdateConfigRequest, UpdateFabricNetworkRequest,
    UpdateFabricVlanRequest, UpdateFirewallRuleRequest, UpdateNetworkIpRequest,
    UpdatePolicyRequest, UpdateRoleRequest, UpdateUserRequest, User, UserAccessKeyPath, UserPath,
    Uuid, VmState, Volume, VolumeAction, VolumeActionQuery, VolumeMount, VolumePath, VolumeSize,
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

    /// Return the account to use in URL paths.
    ///
    /// Mirrors `cloudapi-client`'s `TypedClient::effective_account`: for the
    /// SSH path this is the inner `AuthConfig`'s effective account (honoring
    /// `--act-as`). For the Bearer path it's the `account` captured when the
    /// auth config was constructed -- typically the profile's configured
    /// account. Both branches return a non-empty string on well-formed
    /// configs; callers can treat this as the authoritative source for
    /// `/{account}/*` path construction regardless of auth method.
    pub fn effective_account(&self) -> &str {
        match &self.auth_config.method {
            GatewayAuthMethod::SshKey(cfg) => cfg.effective_account(),
            GatewayAuthMethod::Bearer { account, .. } => account,
        }
    }

    /// Access the inner SSH `AuthConfig` for out-of-band operations
    /// (WebSocket upgrades, raw cloudapi passthrough).
    ///
    /// Returns `None` on the Bearer path; WebSocket handlers that need to
    /// forge their own signed request can use this accessor.
    pub fn ssh_auth_config(&self) -> Option<&triton_auth::AuthConfig> {
        match &self.auth_config.method {
            GatewayAuthMethod::SshKey(cfg) => Some(cfg),
            GatewayAuthMethod::Bearer { .. } => None,
        }
    }
}

// =============================================================================
// Emit-payload mode: capture request payloads without sending
//
// All emit-payload code is compiled only in debug builds (cfg(debug_assertions)).
// Release builds contain none of the fake-response machinery.
//
// This harness is a direct port of the one in `cloudapi-client` so the CLI can
// switch to `triton-gateway-client` as its sole HTTP client without losing the
// comparison-testing path.
// =============================================================================

#[cfg(debug_assertions)]
static EMIT_PAYLOAD_MODE: AtomicBool = AtomicBool::new(false);

/// Enable or disable emit-payload mode.
///
/// When enabled, mutating requests (POST/PUT/DELETE) are intercepted in the
/// `pre` hook, which prints a JSON envelope and aborts with a sentinel error.
/// GET/HEAD requests flow through the `exec` hook, which prints the envelope
/// and returns a fake response so calling code can continue.
#[cfg(debug_assertions)]
pub fn set_emit_payload_mode(enabled: bool) {
    EMIT_PAYLOAD_MODE.store(enabled, Ordering::Relaxed);
}

/// Check whether emit-payload mode is active.
#[cfg(debug_assertions)]
pub fn is_emit_payload_mode() -> bool {
    EMIT_PAYLOAD_MODE.load(Ordering::Relaxed)
}

/// Print a JSON envelope capturing an HTTP request's method, URL, and body.
/// The URL includes the scheme/host so callers can verify which datacenter a
/// request targets (e.g., `triton image copy` sends to a different DC).
#[cfg(debug_assertions)]
fn emit_payload_envelope(method: &str, url: &str, body: serde_json::Value) {
    let envelope = serde_json::json!({
        "method": method,
        "url": url,
        "body": body,
    });

    #[allow(clippy::expect_used)]
    let output =
        serde_json::to_string_pretty(&envelope).expect("JSON serialization should not fail");
    println!("{output}");
}

/// Sentinel error message used to signal that emit-payload mode intercepted
/// the request. Callers (e.g., the CLI) should detect this and exit cleanly.
#[cfg(debug_assertions)]
pub const EMIT_PAYLOAD_SENTINEL: &str = "__payload_emitted__";

/// Print a JSON envelope capturing the request's method, path, and body,
/// then return a sentinel error to abort the request.
#[cfg(debug_assertions)]
#[allow(clippy::result_large_err)]
fn emit_request_payload<E>(request: &reqwest::Request) -> Result<(), Error<E>> {
    let body = request
        .body()
        .and_then(|b| b.as_bytes())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
        .unwrap_or(serde_json::Value::Null);

    emit_payload_envelope(request.method().as_str(), request.url().as_str(), body);

    Err(Error::Custom(EMIT_PAYLOAD_SENTINEL.to_string()))
}

/// In emit-payload mode, emit the GET envelope and return a fake HTTP
/// response so calling code can continue (e.g., share_image does GET →
/// modify ACL → POST).
#[cfg(debug_assertions)]
fn emit_and_fake_get_response(
    request: &reqwest::Request,
    info: &OperationInfo,
) -> reqwest::Result<reqwest::Response> {
    let url = request.url();
    emit_payload_envelope(
        request.method().as_str(),
        url.as_str(),
        serde_json::Value::Null,
    );

    let body_bytes = if info.operation_id.starts_with("list_") {
        load_fixture()
            .as_ref()
            .and_then(|f| {
                f.get("responses")
                    .and_then(|r| r.get(info.operation_id))
                    .or_else(|| f.get("list_default"))
            })
            .and_then(|v| serde_json::to_vec(v).ok())
            .unwrap_or_else(|| b"[]".to_vec())
    } else {
        let last_seg = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .unwrap_or("unknown");
        let last_seg_uuid = uuid::Uuid::try_parse(last_seg).unwrap_or(uuid::Uuid::nil());
        #[allow(clippy::expect_used)]
        let fake_ts: chrono::DateTime<chrono::Utc> = "2025-01-01T00:00:00.000Z"
            .parse()
            .expect("hardcoded RFC3339 timestamp must parse");

        fake_response_body(info.operation_id, last_seg, last_seg_uuid, fake_ts, url)
    };

    #[allow(clippy::expect_used)]
    let http_response = http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(body_bytes)
        .expect("fake HTTP response construction should not fail");
    Ok(reqwest::Response::from(http_response))
}

/// Cached parsed fixture from `TRITON_EMIT_PAYLOAD_FIXTURES` env var.
#[cfg(debug_assertions)]
static FIXTURE_CACHE: OnceLock<Option<serde_json::Value>> = OnceLock::new();

/// Load the emit-payload fixture file if `TRITON_EMIT_PAYLOAD_FIXTURES` is set.
#[cfg(debug_assertions)]
fn load_fixture() -> &'static Option<serde_json::Value> {
    FIXTURE_CACHE.get_or_init(|| {
        let path = std::env::var("TRITON_EMIT_PAYLOAD_FIXTURES").ok()?;
        // arch-lint: allow(no-sync-io) reason="One-shot cache init in debug-only emit-payload mode"
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    })
}

/// Try to build a fake response body from the shared fixture file.
#[cfg(debug_assertions)]
fn fixture_response_body(
    operation_id: &str,
    last_seg: &str,
    last_seg_uuid: uuid::Uuid,
    url: &reqwest::Url,
) -> Option<Vec<u8>> {
    let fixture = load_fixture().as_ref()?;
    let responses = fixture.get("responses")?;

    let template = responses.get(operation_id).or_else(|| {
        let rest = operation_id.strip_prefix("head_")?;
        let key = format!("get_{rest}");
        responses.get(&key)
    })?;

    let mut json_str = serde_json::to_string(template).ok()?;

    let vlan_id: u64 = url
        .path_segments()
        .and_then(|segs| {
            let parts: Vec<_> = segs.collect();
            parts
                .iter()
                .position(|&s| s == "vlans")
                .and_then(|i| parts.get(i + 1))
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(0);

    json_str = json_str.replace("$LAST_SEG_UUID", &last_seg_uuid.to_string());
    json_str = json_str.replace("$LAST_SEG", last_seg);
    json_str = json_str.replace("\"$VLAN_ID\"", &vlan_id.to_string());

    Some(json_str.into_bytes())
}

/// Build a fake response body for a single-resource GET based on operation_id.
#[cfg(debug_assertions)]
#[allow(clippy::expect_used)]
fn fake_response_body(
    operation_id: &str,
    last_seg: &str,
    last_seg_uuid: uuid::Uuid,
    fake_ts: chrono::DateTime<chrono::Utc>,
    url: &reqwest::Url,
) -> Vec<u8> {
    if let Some(bytes) = fixture_response_body(operation_id, last_seg, last_seg_uuid, url) {
        return bytes;
    }

    use cloudapi_api::ImageRequirements;

    match operation_id {
        "get_account" | "head_account" => {
            let fake = Account {
                id: last_seg_uuid,
                login: last_seg.to_string(),
                email: last_seg.to_string(),
                company_name: None,
                first_name: None,
                last_name: None,
                address: None,
                postal_code: None,
                city: None,
                state: None,
                country: None,
                phone: None,
                created: fake_ts,
                updated: fake_ts,
                triton_cns_enabled: None,
            };
            serde_json::to_vec(&fake).expect("Account serialization should not fail")
        }
        "get_image" | "head_image" => {
            let fake = Image {
                id: last_seg_uuid,
                name: last_seg.to_string(),
                version: "0.0.0".to_string(),
                os: "other".to_string(),
                image_type: ImageType::Other,
                requirements: ImageRequirements::default(),
                description: None,
                homepage: None,
                published_at: None,
                owner: None,
                public: None,
                state: None,
                tags: None,
                eula: None,
                acl: None,
                origin: None,
                image_size: None,
                files: None,
                error: None,
                role_tag: None,
            };
            serde_json::to_vec(&fake).expect("Image serialization should not fail")
        }
        "get_machine" | "head_machine" => {
            let fake = Machine {
                id: last_seg_uuid,
                name: last_seg.to_string(),
                machine_type: cloudapi_api::MachineType::Virtualmachine,
                brand: cloudapi_api::VmapiBrand::Bhyve,
                state: MachineState::Running,
                image: uuid::Uuid::nil(),
                package: "emit-payload-fake".to_string(),
                memory: None,
                disk: 0,
                ips: vec![],
                metadata: Default::default(),
                tags: Default::default(),
                created: fake_ts,
                updated: fake_ts,
                networks: None,
                primary_ip: None,
                nics: vec![],
                docker: None,
                firewall_enabled: None,
                deletion_protection: None,
                compute_node: None,
                dns_names: None,
                free_space: None,
                disks: None,
                encrypted: None,
                flexible: None,
                delegate_dataset: None,
                role_tag: None,
            };
            serde_json::to_vec(&fake).expect("Machine serialization should not fail")
        }
        "get_package" | "head_package" => {
            let fake = Package {
                id: last_seg_uuid,
                name: last_seg.to_string(),
                memory: 0,
                disk: 0,
                swap: 0,
                vcpus: 0,
                lwps: None,
                version: None,
                group: None,
                description: None,
                default: false,
                brand: None,
                flexible_disk: None,
                disks: None,
                role_tag: None,
            };
            serde_json::to_vec(&fake).expect("Package serialization should not fail")
        }
        "get_network" | "head_network" | "get_fabric_network" | "head_fabric_network" => {
            let fake = Network {
                id: last_seg_uuid,
                name: last_seg.to_string(),
                public: false,
                fabric: None,
                description: None,
                gateway: None,
                internet_nat: None,
                provision_start_ip: None,
                provision_end_ip: None,
                subnet: None,
                netmask: None,
                vlan_id: None,
                suffixes: None,
                resolvers: None,
                routes: None,
                role_tag: None,
            };
            serde_json::to_vec(&fake).expect("Network serialization should not fail")
        }
        "get_firewall_rule" | "head_firewall_rule" => {
            let fake = FirewallRule {
                id: last_seg_uuid,
                rule: String::new(),
                enabled: false,
                log: false,
                global: None,
                description: None,
                created: None,
                updated: None,
                role_tag: None,
            };
            serde_json::to_vec(&fake).expect("FirewallRule serialization should not fail")
        }
        "get_volume" => {
            let fake: types::Volume = types::Volume::builder()
                .id(last_seg_uuid)
                .name(last_seg.to_string())
                .owner_uuid(uuid::Uuid::nil())
                .type_(types::VolumeType::Tritonnfs)
                .size(0_u64)
                .state(types::VolumeState::Ready)
                .created(fake_ts)
                .try_into()
                .expect("Volume builder should not fail");
            serde_json::to_vec(&fake).expect("Volume serialization should not fail")
        }
        "get_key" | "head_key" | "get_user_key" | "head_user_key" => {
            let fake = SshKey {
                name: last_seg.to_string(),
                key: String::new(),
                fingerprint: String::new(),
                created: None,
                role_tag: None,
            };
            serde_json::to_vec(&fake).expect("SshKey serialization should not fail")
        }
        "get_nic" | "head_nic" => {
            let fake = Nic {
                mac: "00:00:00:00:00:00".to_string(),
                primary: false,
                ip: "0.0.0.0".to_string(),
                netmask: "0.0.0.0".to_string(),
                gateway: None,
                network: uuid::Uuid::nil(),
                state: None,
            };
            serde_json::to_vec(&fake).expect("Nic serialization should not fail")
        }
        "get_machine_snapshot" | "head_machine_snapshot" => {
            let fake = Snapshot {
                name: last_seg.to_string(),
                state: cloudapi_api::SnapshotState::Created,
                created: Some(fake_ts),
                updated: None,
            };
            serde_json::to_vec(&fake).expect("Snapshot serialization should not fail")
        }
        "get_machine_disk" | "head_machine_disk" => {
            let fake = Disk {
                id: last_seg_uuid,
                size: 0,
                block_size: None,
                pci_slot: None,
                boot: None,
                state: None,
            };
            serde_json::to_vec(&fake).expect("Disk serialization should not fail")
        }
        "get_fabric_vlan" | "head_fabric_vlan" => {
            let fake = FabricVlan {
                vlan_id: 0,
                name: last_seg.to_string(),
                description: None,
            };
            serde_json::to_vec(&fake).expect("FabricVlan serialization should not fail")
        }
        _ => {
            let fake = serde_json::json!({
                "id": last_seg,
                "name": last_seg,
            });
            serde_json::to_vec(&fake).expect("fallback serialization should not fail")
        }
    }
}

// =============================================================================
// ClientHooks: intercept create_machine to transform body to legacy format,
// and wire emit-payload behavior into the request pipeline.
//
// Overrides the default (empty) ClientHooks impl on `&Client` via auto-ref
// specialization: the generated code calls `client.pre(...)` where `client`
// is `&Client`, so `&self` resolves to `&Client` (exact match on `Client`)
// before `&&Client` (auto-ref match on `&Client`).
// =============================================================================

impl ClientHooks<GatewayAuthConfig> for Client {
    async fn pre<E>(
        &self,
        request: &mut reqwest::Request,
        info: &OperationInfo,
    ) -> std::result::Result<(), Error<E>> {
        if info.operation_id == "create_machine" {
            transform_create_machine_body(request);
        }

        // Emit-payload mode: intercept mutations (POST/PUT/DELETE/PATCH) here.
        // GETs/HEADs pass through to the `exec` hook which returns fake responses.
        #[cfg(debug_assertions)]
        if EMIT_PAYLOAD_MODE.load(Ordering::Relaxed)
            && request.method() != reqwest::Method::GET
            && request.method() != reqwest::Method::HEAD
        {
            inject_map_params(request, info);
            return emit_request_payload(request);
        }

        Ok(())
    }

    #[cfg(debug_assertions)]
    async fn exec(
        &self,
        request: reqwest::Request,
        info: &OperationInfo,
    ) -> reqwest::Result<reqwest::Response> {
        if EMIT_PAYLOAD_MODE.load(Ordering::Relaxed)
            && (request.method() == reqwest::Method::GET
                || request.method() == reqwest::Method::HEAD)
        {
            return emit_and_fake_get_response(&request, info);
        }
        progenitor_client::ClientInfo::client(self)
            .execute(request)
            .await
    }
}

/// In emit-payload mode, inject path parameters into the request body to
/// match Node.js Restify's `mapParams: true` behavior.
#[cfg(debug_assertions)]
fn inject_map_params(request: &mut reqwest::Request, info: &OperationInfo) {
    if info.operation_id == "create_user_access_key" {
        let uuid = {
            let segments: Vec<&str> = request
                .url()
                .path()
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            segments
                .iter()
                .position(|&s| s == "users")
                .and_then(|pos| segments.get(pos + 1).map(|s| s.to_string()))
        };
        if let Some(uuid) = uuid {
            inject_body_field(request, "userId", &uuid);
        }
    }
}

/// Insert a string field into a JSON request body.
#[cfg(debug_assertions)]
fn inject_body_field(request: &mut reqwest::Request, key: &str, value: &str) {
    let Some(bytes) = request.body().and_then(|b| b.as_bytes()) else {
        return;
    };
    let Ok(mut obj) = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(bytes)
    else {
        return;
    };
    obj.insert(
        key.to_string(),
        serde_json::Value::String(value.to_string()),
    );
    #[allow(clippy::expect_used)]
    let new_bytes = serde_json::to_vec(&obj).expect("re-serialization should not fail");
    let len = new_bytes.len();
    *request.body_mut() = Some(reqwest::Body::from(new_bytes));
    request.headers_mut().insert(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from(len),
    );
}

/// Transform a create_machine request body from structured to legacy format.
///
/// Node.js CloudAPI expects tags as `tag.KEY=VALUE` and metadata as
/// `metadata.KEY=VALUE` as top-level JSON fields, not nested objects.
/// This runs in the `pre` hook after auth headers are already set
/// (HTTP Signature auth signs method+path, not the body).
fn transform_create_machine_body(request: &mut reqwest::Request) {
    let Some(bytes) = request.body().and_then(|b| b.as_bytes()) else {
        return;
    };
    let Ok(mut obj) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return;
    };

    let Some(map) = obj.as_object_mut() else {
        return;
    };

    if let Some(tags) = map.remove("tags")
        && let Some(tags_obj) = tags.as_object()
    {
        for (key, value) in tags_obj {
            map.insert(format!("tag.{key}"), value.clone());
        }
    }

    if let Some(metadata) = map.remove("metadata")
        && let Some(meta_obj) = metadata.as_object()
    {
        for (key, value) in meta_obj {
            map.insert(format!("metadata.{key}"), value.clone());
        }
    }

    if let Some(networks) = map.get_mut("networks")
        && let Some(arr) = networks.as_array_mut()
    {
        for entry in arr.iter_mut() {
            if let Some(obj) = entry.as_object()
                && obj.len() == 1
                && let Some(uuid_val) = obj.get("ipv4_uuid")
            {
                *entry = uuid_val.clone();
            }
        }
    }

    #[allow(clippy::expect_used)]
    let new_bytes = serde_json::to_vec(&obj).expect("re-serialization should not fail");
    let len = new_bytes.len();
    *request.body_mut() = Some(reqwest::Body::from(new_bytes));
    request.headers_mut().insert(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from(len),
    );
}

// =============================================================================
// TypedClient: ergonomic wrappers (ported from cloudapi-client)
// =============================================================================

/// Filter options for listing machines.
#[derive(Debug, Default, Clone)]
pub struct ListMachinesFilter {
    pub name: Option<String>,
    pub state: Option<types::MachineState>,
    pub image: Option<Uuid>,
    pub memory: Option<u64>,
    pub machine_type: Option<types::MachineType>,
    pub brand: Option<types::VmBrand>,
    pub docker: Option<bool>,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
    pub tag: Option<(String, String)>,
}

/// Error type for the `TypedClient::get_machine` 410-Gone-aware wrapper.
#[derive(Debug, thiserror::Error)]
pub enum GetMachineError {
    /// Progenitor client error (auth, transport, server errors)
    #[error("{0}")]
    Client(String),
    /// Failed to parse response body as a Machine during 410 recovery
    #[error("failed to parse JSON: {error}\nResponse body: {body}")]
    JsonParse { error: String, body: String },
    /// Recovered a Machine from InvalidResponsePayload but state was not Deleted,
    /// meaning this was likely not a 410 Gone response
    #[error("unexpected machine state during 410 recovery: {state}\nResponse body: {body}")]
    UnexpectedRecovery {
        state: types::MachineState,
        body: String,
    },
    /// Machine not found (404)
    #[error("machine not found")]
    NotFound,
}

/// Wrapper that adds an `action` field to a request body for action-dispatch
/// endpoints. Node.js CloudAPI expects action in the body: `{"action": "stop"}`.
#[derive(serde::Serialize)]
struct ActionBody<A: serde::Serialize, B: serde::Serialize> {
    action: A,
    #[serde(flatten)]
    body: B,
}

/// Serialize a request type to JSON Value, panicking on failure.
#[allow(clippy::expect_used)]
fn to_json_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("request serialization should not fail")
}

impl TypedClient {
    // ========================================================================
    // Machine Creation (body transformation handled by ClientHooks pre-hook)
    // ========================================================================

    /// Create a machine with legacy-compatible format
    ///
    /// Node.js CloudAPI expects tags as `tag.KEY=VALUE` and metadata as
    /// `metadata.KEY=VALUE` as top-level JSON fields, not nested objects.
    /// The `ClientHooks::pre` hook on `Client` transparently transforms the
    /// request body before it is sent.
    pub async fn create_machine(
        &self,
        account: &str,
        request: &types::CreateMachineRequest,
    ) -> Result<types::Machine, Error<types::Error>> {
        self.inner
            .create_machine()
            .account(account)
            .body(request.clone())
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// List machines with tag filtering.
    pub async fn list_machines_with_tags(
        &self,
        account: &str,
        filter: &ListMachinesFilter,
    ) -> Result<Vec<types::Machine>, Error<types::Error>> {
        let mut builder = self.inner.list_machines().account(account);

        if let Some(name) = &filter.name {
            builder = builder.name(name.clone());
        }
        if let Some(state) = &filter.state {
            builder = builder.state(*state);
        }
        if let Some(image) = &filter.image {
            builder = builder.image(*image);
        }
        if let Some(memory) = filter.memory {
            builder = builder.memory(memory);
        }
        if let Some(machine_type) = &filter.machine_type {
            builder = builder.type_(*machine_type);
        }
        if let Some(brand) = &filter.brand {
            builder = builder.brand(*brand);
        }
        if let Some(docker) = filter.docker {
            builder = builder.docker(docker);
        }
        if let Some(offset) = filter.offset {
            builder = builder.offset(offset);
        }
        if let Some(limit) = filter.limit {
            builder = builder.limit(limit);
        }

        if let Some((key, value)) = &filter.tag {
            builder = builder.tag(format!("{key}={value}"));
        }

        builder.send().await.map(|r| r.into_inner())
    }

    // ========================================================================
    // Machine Retrieval (with 410 handling)
    // ========================================================================

    /// Get a machine by UUID, with 410-Gone recovery.
    ///
    /// This wraps the Progenitor-generated `get_machine` to handle the CloudAPI
    /// quirk where deleted machines return HTTP 410 Gone with a Machine body
    /// (not an Error body). Progenitor treats 4xx as errors and fails to parse
    /// the Machine body, so we catch the parse failure and recover.
    pub async fn get_machine(
        &self,
        account: &str,
        machine: &Uuid,
    ) -> Result<types::Machine, GetMachineError> {
        match self
            .inner
            .get_machine()
            .account(account)
            .machine(machine.to_string())
            .send()
            .await
        {
            Ok(rv) => Ok(rv.into_inner()),
            Err(Error::InvalidResponsePayload(bytes, _)) => {
                let machine: types::Machine =
                    serde_json::from_slice(&bytes).map_err(|e| GetMachineError::JsonParse {
                        error: e.to_string(),
                        body: String::from_utf8_lossy(&bytes).to_string(),
                    })?;
                if machine.state == types::MachineState::Deleted {
                    Ok(machine)
                } else {
                    Err(GetMachineError::UnexpectedRecovery {
                        state: machine.state,
                        body: String::from_utf8_lossy(&bytes).to_string(),
                    })
                }
            }
            Err(Error::ErrorResponse(rv)) if rv.status() == reqwest::StatusCode::NOT_FOUND => {
                Err(GetMachineError::NotFound)
            }
            Err(e) => Err(GetMachineError::Client(e.to_string())),
        }
    }

    // ========================================================================
    // Machine Actions
    // ========================================================================

    /// Start a machine.
    pub async fn start_machine(
        &self,
        account: &str,
        machine: &Uuid,
        request: &StartMachineRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Start,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    /// Stop a machine.
    pub async fn stop_machine(
        &self,
        account: &str,
        machine: &Uuid,
        request: &StopMachineRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Stop,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    /// Reboot a machine.
    pub async fn reboot_machine(
        &self,
        account: &str,
        machine: &Uuid,
        request: &RebootMachineRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Reboot,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    /// Resize a machine to a different package.
    pub async fn resize_machine(
        &self,
        account: &str,
        machine: &Uuid,
        request: &ResizeMachineRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Resize,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    /// Rename a machine.
    pub async fn rename_machine(
        &self,
        account: &str,
        machine: &Uuid,
        request: &RenameMachineRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Rename,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    /// Enable firewall for a machine.
    pub async fn enable_firewall(
        &self,
        account: &str,
        machine: &Uuid,
        request: &EnableFirewallRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::EnableFirewall,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    /// Disable firewall for a machine.
    pub async fn disable_firewall(
        &self,
        account: &str,
        machine: &Uuid,
        request: &DisableFirewallRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::DisableFirewall,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    /// Enable deletion protection for a machine.
    pub async fn enable_deletion_protection(
        &self,
        account: &str,
        machine: &Uuid,
        request: &EnableDeletionProtectionRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::EnableDeletionProtection,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    /// Disable deletion protection for a machine.
    pub async fn disable_deletion_protection(
        &self,
        account: &str,
        machine: &Uuid,
        request: &DisableDeletionProtectionRequest,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::DisableDeletionProtection,
            body: request,
        };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|_| ())
    }

    // ========================================================================
    // Image Actions
    // ========================================================================

    /// Update image metadata.
    pub async fn update_image_metadata(
        &self,
        account: &str,
        dataset: &Uuid,
        request: &UpdateImageRequest,
    ) -> Result<types::Image, Error<types::Error>> {
        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::Update)
            .body(to_json_value(request))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Export image to Manta.
    pub async fn export_image(
        &self,
        account: &str,
        dataset: &Uuid,
        request: &ExportImageRequest,
    ) -> Result<types::Image, Error<types::Error>> {
        let body = ActionBody {
            action: ImageAction::Export,
            body: request,
        };
        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Clone image to account.
    pub async fn clone_image(
        &self,
        account: &str,
        dataset: &Uuid,
    ) -> Result<types::Image, Error<types::Error>> {
        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::Clone)
            .body(serde_json::json!({}))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Import image from another datacenter.
    pub async fn import_image_from_datacenter(
        &self,
        account: &str,
        datacenter: &str,
        id: Uuid,
    ) -> Result<types::Image, Error<types::Error>> {
        self.inner
            .create_or_import_image()
            .account(account)
            .action(types::ImageAction::ImportFromDatacenter)
            .datacenter(datacenter)
            .id(id)
            .body(serde_json::json!({}))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Share image with another account.
    pub async fn share_image(
        &self,
        account: &str,
        dataset: &Uuid,
        target_account: Uuid,
    ) -> Result<types::Image, Error<types::Error>> {
        let image = self
            .inner
            .get_image()
            .account(account)
            .dataset(dataset.to_string())
            .send()
            .await?
            .into_inner();

        let mut acl = image.acl.unwrap_or_default();
        if !acl.contains(&target_account) {
            acl.push(target_account);
        }

        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::Update)
            .body(serde_json::json!({"acl": acl}))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Unshare image from another account.
    pub async fn unshare_image(
        &self,
        account: &str,
        dataset: &Uuid,
        target_account: Uuid,
    ) -> Result<types::Image, Error<types::Error>> {
        let image = self
            .inner
            .get_image()
            .account(account)
            .dataset(dataset.to_string())
            .send()
            .await?
            .into_inner();

        let mut acl = image.acl.unwrap_or_default();
        acl.retain(|a| a != &target_account);

        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::Update)
            .body(serde_json::json!({"acl": acl}))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    // ========================================================================
    // Volume Actions
    // ========================================================================

    /// Get a volume by UUID.
    pub async fn get_volume(
        &self,
        account: &str,
        id: &str,
    ) -> Result<types::Volume, Error<types::Error>> {
        Ok(self
            .inner
            .get_volume()
            .account(account)
            .id(id)
            .send()
            .await?
            .into_inner())
    }

    /// List volumes.
    pub async fn list_volumes(
        &self,
        account: &str,
    ) -> Result<Vec<types::Volume>, Error<types::Error>> {
        Ok(self
            .inner
            .list_volumes()
            .account(account)
            .send()
            .await?
            .into_inner())
    }

    /// Create a volume.
    pub async fn create_volume(
        &self,
        account: &str,
        request: types::CreateVolumeRequest,
    ) -> Result<types::Volume, Error<types::Error>> {
        Ok(self
            .inner
            .create_volume()
            .account(account)
            .body(request)
            .send()
            .await?
            .into_inner())
    }

    /// Update volume name.
    pub async fn update_volume_name(
        &self,
        account: &str,
        id: &Uuid,
        request: &UpdateVolumeRequest,
    ) -> Result<types::Volume, Error<types::Error>> {
        let body = ActionBody {
            action: VolumeAction::Update,
            body: request,
        };
        self.inner
            .update_volume()
            .account(account)
            .id(id.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    // ========================================================================
    // Disk Actions
    // ========================================================================

    /// Resize a machine disk.
    pub async fn resize_disk(
        &self,
        account: &str,
        machine: &Uuid,
        disk: &Uuid,
        request: &ResizeDiskRequest,
    ) -> Result<types::Disk, Error<types::Error>> {
        let body = ActionBody {
            action: DiskAction::Resize,
            body: request,
        };
        self.inner
            .resize_machine_disk()
            .account(account)
            .machine(machine.to_string())
            .disk(disk.to_string())
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }
}
