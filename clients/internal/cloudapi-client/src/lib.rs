// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton CloudAPI Client Library
//!
//! This client provides typed access to the Triton CloudAPI service.
//! CloudAPI is the public-facing REST API for managing virtual machines,
//! images, networks, volumes, and other resources in Triton.
//!
//! ## Usage
//!
//! ### Authenticated Client (Recommended)
//!
//! For authenticated requests using HTTP Signature authentication:
//!
//! ```ignore
//! use cloudapi_client::{AuthenticatedClient, AuthConfig, KeySource};
//!
//! // Configure authentication
//! let auth_config = AuthConfig::new(
//!     "myaccount",
//!     KeySource::auto("aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99"),
//! );
//!
//! // Create authenticated client
//! let client = AuthenticatedClient::new("https://cloudapi.example.com", auth_config);
//!
//! // All requests automatically include Date and Authorization headers
//! let machines = client.inner().list_machines().account("myaccount").send().await?;
//! ```
//!
//! ### TypedClient for Action-based Endpoints
//!
//! For action-based endpoints (machines, images, volumes, disks), use the
//! typed wrapper methods for better ergonomics:
//!
//! ```ignore
//! use cloudapi_client::{TypedClient, AuthConfig, KeySource};
//!
//! let auth_config = AuthConfig::new(
//!     "myaccount",
//!     KeySource::auto("aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99"),
//! );
//!
//! let client = TypedClient::new("https://cloudapi.example.com", auth_config);
//!
//! // Typed machine actions
//! client.start_machine("myaccount", &machine_uuid, None).await?;
//! client.stop_machine("myaccount", &machine_uuid, None).await?;
//! client.resize_machine("myaccount", &machine_uuid, "new-package", None).await?;
//!
//! // Access underlying client for other operations
//! let account = client.inner().get_account().account("myaccount").send().await?;
//! ```

pub mod auth;
pub mod pagination;

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

#[cfg(debug_assertions)]
use std::sync::OnceLock;
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, Ordering};

use progenitor_client::{ClientHooks, OperationInfo};

// Re-export triton-auth types for convenience
pub use triton_auth::{AuthConfig, AuthError, KeySource};

// Re-export types from the API crate for convenience
pub use cloudapi_api::{
    // Key types
    AccessKey,
    AccessKeyPath,
    AccessKeyStatus,
    // Account types
    Account,
    AccountPath,
    // Misc types
    AddForeignDatacenterRequest,
    // Machine resources
    AddMetadataRequest,
    // Network types
    AddNicRequest,
    // Machine types
    AuditEntry,
    // User types
    ChangePasswordRequest,
    // Changefeed types
    ChangefeedChangeKind,
    ChangefeedMessage,
    ChangefeedResource,
    ChangefeedSubResource,
    ChangefeedSubscription,
    // Image types
    CloneImageRequest,
    Config,
    CreateAccessKeyRequest,
    CreateAccessKeyResponse,
    CreateDiskRequest,
    CreateFabricNetworkRequest,
    CreateFabricVlanRequest,
    // Firewall types
    CreateFirewallRuleRequest,
    CreateImageRequest,
    CreateMachineRequest,
    CreatePolicyRequest,
    CreateRoleRequest,
    CreateSnapshotRequest,
    CreateSshKeyRequest,
    CreateUserRequest,
    // Volume types
    CreateVolumeRequest,
    CredentialType,
    Datacenter,
    Datacenters,
    DisableDeletionProtectionRequest,
    DisableFirewallRequest,
    Disk,
    DiskAction,
    DiskActionQuery,
    DiskPath,
    DiskSpec,
    DiskState,
    EnableDeletionProtectionRequest,
    EnableFirewallRequest,
    ExportImageRequest,
    FabricNetworkPath,
    FabricVlan,
    FabricVlanPath,
    FirewallRule,
    FirewallRulePath,
    Image,
    ImageAction,
    ImageActionQuery,
    ImageCollectionActionQuery,
    ImagePath,
    ImageState,
    ImageType,
    ImportImageRequest,
    KeyPath,
    ListImagesQuery,
    ListMachinesQuery,
    Machine,
    MachineAction,
    MachineActionQuery,
    MachineNic,
    MachinePath,
    MachineState,
    // Common types
    Metadata,
    MetadataKeyPath,
    MigrateRequest,
    Migration,
    MigrationAction,
    MigrationEstimate,
    MigrationEstimateRequest,
    MigrationPhase,
    MigrationState,
    MountMode,
    Network,
    NetworkIp,
    NetworkIpPath,
    NetworkObject,
    NetworkPath,
    Nic,
    NicPath,
    NicState,
    Package,
    PackagePath,
    Policy,
    PolicyPath,
    ProvisioningLimit,
    ProvisioningLimits,
    RebootMachineRequest,
    RenameMachineRequest,
    ReplaceRoleTagsRequest,
    ResizeDiskRequest,
    ResizeMachineRequest,
    Role,
    RolePath,
    Service,
    Services,
    // Share/Unshare: client-side ACL logic (no dedicated request types)
    Snapshot,
    SnapshotPath,
    SnapshotState,
    SshKey,
    StartMachineRequest,
    StopMachineRequest,
    TagPath,
    Tags,
    TagsRequest,
    Timestamp,
    UpdateAccessKeyRequest,
    UpdateAccountRequest,
    UpdateConfigRequest,
    UpdateFabricNetworkRequest,
    UpdateFabricVlanRequest,
    UpdateFirewallRuleRequest,
    UpdateImageRequest,
    UpdateNetworkIpRequest,
    UpdatePolicyRequest,
    UpdateRoleRequest,
    UpdateUserRequest,
    UpdateVolumeRequest,
    User,
    UserAccessKeyPath,
    UserPath,
    Uuid,
    // VMAPI types (re-exported through cloudapi-api)
    VmState,
    Volume,
    VolumeAction,
    VolumeActionQuery,
    VolumeMount,
    VolumePath,
    VolumeSize,
};

// =============================================================================
// Emit-payload mode: capture request payloads without sending
//
// All emit-payload code is compiled only in debug builds (cfg(debug_assertions)).
// Release builds contain none of the fake-response machinery.
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
    // Extract body as JSON value (or null if no body)
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
///
/// Dispatches to typed fake response builders per `operation_id`, using
/// proper typed structs so the compiler enforces valid enum values.
/// List operations (operation_id starts with `list_`) get `[]` so
/// resolve-by-name fails gracefully. Unknown operations fall back to
/// a minimal `{id: lastSeg, name: lastSeg}` JSON object.
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

    // HEAD responses are now ResponseValue<ByteStream> with no body parsing,
    // so return an empty 200 response immediately.
    if request.method() == reqwest::Method::HEAD {
        #[allow(clippy::expect_used)]
        let http_response = http::Response::builder()
            .status(200)
            .body(Vec::new())
            .expect("fake HTTP response construction should not fail");
        return Ok(reqwest::Response::from(http_response));
    }

    let body_bytes = if info.operation_id.starts_with("list_") {
        // List endpoints: check responses for a specific list fixture first,
        // then fall back to list_default, then empty array
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
        let fake_ts = chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00.000Z")
            .expect("static timestamp must parse")
            .with_timezone(&chrono::Utc);

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
/// `None` means the env var is unset or the file could not be loaded.
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
/// Returns `Some(bytes)` if the fixture has a matching entry, `None` otherwise.
#[cfg(debug_assertions)]
fn fixture_response_body(
    operation_id: &str,
    last_seg: &str,
    last_seg_uuid: uuid::Uuid,
    url: &reqwest::Url,
) -> Option<Vec<u8>> {
    let fixture = load_fixture().as_ref()?;
    let responses = fixture.get("responses")?;

    let template = responses.get(operation_id)?;

    // Interpolate placeholders on the raw JSON string
    let mut json_str = serde_json::to_string(template).ok()?;

    // Extract VLAN ID from URL path (e.g., /fabrics/.../vlans/100)
    let vlan_id: u64 = url
        .path_segments()
        .and_then(|segs| {
            let parts: Vec<_> = segs.collect();
            // Find "vlans" segment and take the next one
            parts
                .iter()
                .position(|&s| s == "vlans")
                .and_then(|i| parts.get(i + 1))
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(0);

    // Order matters: replace $LAST_SEG_UUID before $LAST_SEG to avoid partial match
    json_str = json_str.replace("$LAST_SEG_UUID", &last_seg_uuid.to_string());
    json_str = json_str.replace("$LAST_SEG", last_seg);
    // $VLAN_ID: replace quoted string with bare integer
    json_str = json_str.replace("\"$VLAN_ID\"", &vlan_id.to_string());

    Some(json_str.into_bytes())
}

/// Build a fake response body for a single-resource GET based on operation_id.
///
/// If `TRITON_EMIT_PAYLOAD_FIXTURES` is set, loads from the shared fixture file.
/// Otherwise falls back to hardcoded typed structs so enum values are
/// compiler-checked. None of the response types use `deny_unknown_fields`,
/// so extra fields from the fallback case are silently ignored.
#[cfg(debug_assertions)]
#[allow(clippy::expect_used)]
fn fake_response_body(
    operation_id: &str,
    last_seg: &str,
    last_seg_uuid: uuid::Uuid,
    fake_ts: chrono::DateTime<chrono::Utc>,
    url: &reqwest::Url,
) -> Vec<u8> {
    // Try shared fixture file first
    if let Some(bytes) = fixture_response_body(operation_id, last_seg, last_seg_uuid, url) {
        return bytes;
    }

    use cloudapi_api::ImageRequirements;

    match operation_id {
        "get_account" => {
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
        "get_image" => {
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
        "get_machine" => {
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
        "get_package" => {
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
        "get_network" | "get_fabric_network" => {
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
        "get_firewall_rule" => {
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
        "get_key" | "get_user_key" => {
            let fake = SshKey {
                name: last_seg.to_string(),
                key: String::new(),
                fingerprint: String::new(),
                created: None,
                role_tag: None,
            };
            serde_json::to_vec(&fake).expect("SshKey serialization should not fail")
        }
        "get_nic" => {
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
        "get_machine_snapshot" => {
            let fake = Snapshot {
                name: last_seg.to_string(),
                state: cloudapi_api::SnapshotState::Created,
                created: Some(fake_ts),
                updated: None,
            };
            serde_json::to_vec(&fake).expect("Snapshot serialization should not fail")
        }
        "get_machine_disk" => {
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
        "get_fabric_vlan" => {
            let fake = FabricVlan {
                vlan_id: 0,
                name: last_seg.to_string(),
                description: None,
            };
            serde_json::to_vec(&fake).expect("FabricVlan serialization should not fail")
        }
        _ => {
            // Fallback: minimal JSON for unhandled operations
            let fake = serde_json::json!({
                "id": last_seg,
                "name": last_seg,
            });
            serde_json::to_vec(&fake).expect("fallback serialization should not fail")
        }
    }
}

// =============================================================================
// ClientHooks: intercept create_machine to transform body to legacy format
// =============================================================================

/// Override the default (empty) `ClientHooks` impl on `&Client` via auto-ref
/// specialization: the generated code calls `client.pre(...)` where `client`
/// is `&Client`, so `&self` resolves to `&Client` (exact match on `Client`)
/// before `&&Client` (auto-ref match on `&Client`).
impl ClientHooks<triton_auth::AuthConfig> for Client {
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
            // Inject path params into body to match Node.js Restify mapParams
            inject_map_params(request, info);
            return emit_request_payload(request);
        }

        Ok(())
    }

    // Override exec to return fake responses for GETs in emit-payload mode
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
/// match Node.js Restify's `mapParams: true` behavior, which merges URL
/// path params into the body. This is only needed for test comparison;
/// the server ignores the redundant fields.
#[cfg(debug_assertions)]
fn inject_map_params(request: &mut reqwest::Request, info: &OperationInfo) {
    // POST /{account}/users/{uuid}/accesskeys
    // Node includes userId in body due to mapParams
    if info.operation_id == "create_user_access_key" {
        let uuid = {
            let segments: Vec<&str> = request
                .url()
                .path()
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            // segments: [account, "users", uuid, "accesskeys"]
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

    // Flatten tags: {"tags": {"k": "v"}} → {"tag.k": "v"}
    if let Some(tags) = map.remove("tags")
        && let Some(tags_obj) = tags.as_object()
    {
        for (key, value) in tags_obj {
            map.insert(format!("tag.{key}"), value.clone());
        }
    }

    // Flatten metadata: {"metadata": {"k": "v"}} → {"metadata.k": "v"}
    if let Some(metadata) = map.remove("metadata")
        && let Some(meta_obj) = metadata.as_object()
    {
        for (key, value) in meta_obj {
            map.insert(format!("metadata.{key}"), value.clone());
        }
    }

    // Simplify networks: NetworkObjects with only ipv4_uuid (no ipv4_ips,
    // no primary) become plain UUID strings to match node-triton's wire format.
    // Objects with additional fields are kept as-is.
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

    // Replace the body and update Content-Length
    #[allow(clippy::expect_used)]
    let new_bytes = serde_json::to_vec(&obj).expect("re-serialization should not fail");
    let len = new_bytes.len();
    *request.body_mut() = Some(reqwest::Body::from(new_bytes));
    request.headers_mut().insert(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from(len),
    );
}

/// Authenticated client wrapper
///
/// This wrapper provides access to a CloudAPI client configured with
/// HTTP Signature authentication. All requests automatically include
/// the required Date and Authorization headers.
pub struct AuthenticatedClient {
    inner: Client,
    auth_config: AuthConfig,
}

impl AuthenticatedClient {
    /// Create a new authenticated client
    ///
    /// # Arguments
    /// * `base_url` - CloudAPI base URL (e.g., "https://cloudapi.example.com")
    /// * `auth_config` - Authentication configuration
    pub fn new(base_url: &str, auth_config: AuthConfig) -> Self {
        Self {
            inner: Client::new_with_client(base_url, reqwest::Client::new(), auth_config.clone()),
            auth_config,
        }
    }

    /// Access the underlying Progenitor client
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    /// Get the authentication configuration
    pub fn auth_config(&self) -> &AuthConfig {
        &self.auth_config
    }
}

/// Filter options for listing machines
#[derive(Debug, Default, Clone)]
pub struct ListMachinesFilter {
    /// Filter by machine name
    pub name: Option<String>,
    /// Filter by state
    pub state: Option<types::MachineState>,
    /// Filter by image UUID
    pub image: Option<Uuid>,
    /// Filter by memory (MB)
    pub memory: Option<u64>,
    /// Filter by machine type
    pub machine_type: Option<types::MachineType>,
    /// Filter by brand
    pub brand: Option<types::VmBrand>,
    /// Filter by docker flag
    pub docker: Option<bool>,
    /// Pagination offset
    pub offset: Option<u64>,
    /// Pagination limit
    pub limit: Option<u64>,
    /// Filter by tag (key, value)
    pub tag: Option<(String, String)>,
}

/// Typed client wrapper for endpoints requiring special handling
///
/// This wrapper provides ergonomic methods for CloudAPI endpoints that need
/// logic beyond the Progenitor-generated client, including action-dispatch
/// endpoints (machines, images, volumes, disks), legacy request formats,
/// tag query encoding, error recovery, and multi-step operations like
/// share/unshare.
///
/// This client is authenticated and will automatically sign all requests.
pub struct TypedClient {
    inner: Client,
    auth_config: AuthConfig,
    http_client: reqwest::Client,
}

impl TypedClient {
    /// Create a new typed client wrapper with authentication
    ///
    /// # Arguments
    /// * `base_url` - CloudAPI base URL (e.g., "https://cloudapi.example.com")
    /// * `auth_config` - Authentication configuration
    pub fn new(base_url: &str, auth_config: AuthConfig) -> Self {
        let http_client = reqwest::Client::new();
        Self {
            inner: Client::new_with_client(base_url, http_client.clone(), auth_config.clone()),
            auth_config,
            http_client,
        }
    }

    /// Create a new typed client with optional TLS certificate validation bypass
    ///
    /// # Arguments
    /// * `base_url` - CloudAPI base URL (e.g., "https://cloudapi.example.com")
    /// * `auth_config` - Authentication configuration
    /// * `insecure` - If true, skip TLS certificate validation (use with caution)
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be built.
    pub fn new_with_insecure(
        base_url: &str,
        auth_config: AuthConfig,
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

    /// Create a new typed client with a pre-built HTTP client
    ///
    /// This allows the caller to control how the `reqwest::Client` is built,
    /// including custom TLS configuration or certificate loading.
    ///
    /// # Arguments
    /// * `base_url` - CloudAPI base URL (e.g., "https://cloudapi.example.com")
    /// * `auth_config` - Authentication configuration
    /// * `http_client` - Pre-built reqwest HTTP client
    pub fn new_with_http_client(
        base_url: &str,
        auth_config: AuthConfig,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            inner: Client::new_with_client(base_url, http_client.clone(), auth_config.clone()),
            auth_config,
            http_client,
        }
    }

    /// Access the underlying Progenitor client for non-wrapped methods
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    /// Get the authentication configuration
    pub fn auth_config(&self) -> &AuthConfig {
        &self.auth_config
    }

    /// Return the account to use in URL paths.
    ///
    /// When `--act-as` is active, this returns the target account so that
    /// URL segments use `/:target_account/` instead of the signing account.
    pub fn effective_account(&self) -> &str {
        self.auth_config.effective_account()
    }

    /// Access the underlying reqwest HTTP client
    ///
    /// This shares the same TLS configuration (e.g., insecure mode)
    /// as the Progenitor-generated client.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Get the base URL this client is connected to
    pub fn baseurl(&self) -> &str {
        use progenitor_client::ClientInfo;
        self.inner.baseurl()
    }

    // ========================================================================
    // Machine Creation (body transformation handled by ClientHooks pre-hook)
    // ========================================================================

    /// Create a machine with legacy-compatible format
    ///
    /// Node.js CloudAPI expects tags as `tag.KEY=VALUE` and metadata as
    /// `metadata.KEY=VALUE` as top-level JSON fields, not nested objects.
    /// The `ClientHooks::pre` hook on `Client` transparently transforms the
    /// request body before it is sent.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `request` - Machine creation request
    ///
    /// # Errors
    /// Returns an error if the request fails or the server returns an error.
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

    /// List machines with tag filtering
    ///
    /// This method handles tag filtering in a way that's compatible with the
    /// Node.js CloudAPI server. The tag filter format is `key=value`.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `filter` - Optional filter options
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

        // Handle tag filtering - Node.js CloudAPI expects tag.KEY=VALUE format
        // but our API trait has a single `tag` field with key=value format
        if let Some((key, value)) = &filter.tag {
            builder = builder.tag(format!("{key}={value}"));
        }

        builder.send().await.map(|r| r.into_inner())
    }

    // ========================================================================
    // Machine Retrieval (with 410 handling)
    // ========================================================================

    /// Get a machine by UUID
    ///
    /// This wraps the Progenitor-generated `get_machine` to handle the CloudAPI
    /// quirk where deleted machines return HTTP 410 Gone with a Machine body
    /// (not an Error body). Progenitor treats 4xx as errors and fails to parse
    /// the Machine body, so we catch the parse failure and recover.
    ///
    /// Because `InvalidResponsePayload` doesn't carry the HTTP status code,
    /// we validate that the recovered Machine has `state == Deleted` to confirm
    /// this was actually a 410 Gone response and not some other parse failure.
    ///
    /// # Returns
    /// Returns the Machine. If the machine is deleted, `machine.state` will be
    /// `MachineState::Deleted`. Check the state to determine if the machine
    /// is still active.
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
            // 410 Gone returns a Machine body, which Progenitor fails to
            // deserialize as types::Error → surfaces as InvalidResponsePayload
            // with the raw bytes still intact. Since InvalidResponsePayload
            // doesn't carry the HTTP status code, we validate that the parsed
            // Machine is in Deleted state to confirm this was a 410 response.
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

    /// Start a machine
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn start_machine(
        &self,
        account: &str,
        machine: &Uuid,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Start,
            body: StartMachineRequest { origin },
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

    /// Stop a machine
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn stop_machine(
        &self,
        account: &str,
        machine: &Uuid,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Stop,
            body: StopMachineRequest { origin },
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

    /// Reboot a machine
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn reboot_machine(
        &self,
        account: &str,
        machine: &Uuid,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Reboot,
            body: RebootMachineRequest { origin },
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

    /// Resize a machine to a different package
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `package` - New package name or UUID
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn resize_machine(
        &self,
        account: &str,
        machine: &Uuid,
        package: String,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Resize,
            body: ResizeMachineRequest { package, origin },
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

    /// Rename a machine
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `name` - New machine alias/name (max 189 chars, or 63 if CNS enabled)
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn rename_machine(
        &self,
        account: &str,
        machine: &Uuid,
        name: String,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::Rename,
            body: RenameMachineRequest { name, origin },
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

    /// Enable firewall for a machine
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn enable_firewall(
        &self,
        account: &str,
        machine: &Uuid,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::EnableFirewall,
            body: EnableFirewallRequest { origin },
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

    /// Disable firewall for a machine
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn disable_firewall(
        &self,
        account: &str,
        machine: &Uuid,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::DisableFirewall,
            body: DisableFirewallRequest { origin },
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

    /// Enable deletion protection for a machine
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn enable_deletion_protection(
        &self,
        account: &str,
        machine: &Uuid,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::EnableDeletionProtection,
            body: EnableDeletionProtectionRequest { origin },
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

    /// Disable deletion protection for a machine
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `origin` - Optional origin identifier (defaults to 'cloudapi')
    pub async fn disable_deletion_protection(
        &self,
        account: &str,
        machine: &Uuid,
        origin: Option<String>,
    ) -> Result<(), Error<types::Error>> {
        let body = ActionBody {
            action: MachineAction::DisableDeletionProtection,
            body: DisableDeletionProtectionRequest { origin },
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

    /// Update image metadata
    ///
    /// Node-triton sends `?action=update` as a query parameter with the
    /// update fields in the body. We match that wire format here.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `dataset` - Image UUID
    /// * `request` - Image update request with fields to change
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

    /// Export image to Manta
    ///
    /// Node-triton sends `action` in the request body alongside `manta_path`,
    /// so we use `ActionBody` here to match that wire format.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `dataset` - Image UUID
    /// * `manta_path` - Manta path for export destination
    pub async fn export_image(
        &self,
        account: &str,
        dataset: &Uuid,
        manta_path: String,
    ) -> Result<types::Image, Error<types::Error>> {
        let body = ActionBody {
            action: ImageAction::Export,
            body: ExportImageRequest { manta_path },
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

    /// Clone image to account
    ///
    /// Node-triton sends `?action=clone` as a query parameter with an empty
    /// body. We match that wire format here.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `dataset` - Image UUID
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

    /// Import image from another datacenter
    ///
    /// Node-triton sends `POST /{account}/images?action=import-from-datacenter&datacenter=X&id=Y`
    /// to the collection endpoint with an empty body. We match that wire format.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `datacenter` - Source datacenter name
    /// * `id` - Image UUID in source datacenter
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

    /// Share image with another account
    ///
    /// Matches node-triton's pattern: GET the image to read its current ACL,
    /// add the target account, then POST an `update` action with the full ACL.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `dataset` - Image UUID
    /// * `target_account` - Account UUID to share with
    pub async fn share_image(
        &self,
        account: &str,
        dataset: &Uuid,
        target_account: Uuid,
    ) -> Result<types::Image, Error<types::Error>> {
        // 1. GET image to read current ACL
        // (in emit-payload mode, the exec hook returns a fake Image with acl: None)
        let image = self
            .inner
            .get_image()
            .account(account)
            .dataset(dataset.to_string())
            .send()
            .await?
            .into_inner();

        // 2. Add target to ACL (if not already present)
        let mut acl: Vec<Uuid> = image.acl.unwrap_or_default();
        if !acl.contains(&target_account) {
            acl.push(target_account);
        }

        // 3. POST update with modified ACL
        // (in emit-payload mode the pre hook emits this and returns sentinel)
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

    /// Unshare image from another account
    ///
    /// Matches node-triton's pattern: GET the image to read its current ACL,
    /// remove the target account, then POST an `update` action with the modified ACL.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `dataset` - Image UUID
    /// * `target_account` - Account UUID to unshare from
    pub async fn unshare_image(
        &self,
        account: &str,
        dataset: &Uuid,
        target_account: Uuid,
    ) -> Result<types::Image, Error<types::Error>> {
        // 1. GET image to read current ACL
        // (in emit-payload mode, the exec hook returns a fake Image with acl: None)
        let image = self
            .inner
            .get_image()
            .account(account)
            .dataset(dataset.to_string())
            .send()
            .await?
            .into_inner();

        // 2. Remove target from ACL
        let mut acl: Vec<Uuid> = image.acl.unwrap_or_default();
        acl.retain(|a| a != &target_account);

        // 3. POST update with modified ACL
        // (in emit-payload mode the pre hook emits this and returns sentinel)
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

    /// Get a volume by UUID
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

    /// List volumes
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

    /// Create a volume
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

    /// Update volume name
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `id` - Volume UUID
    /// * `request` - Volume update request
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

    /// Resize a machine disk
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    /// * `disk` - Disk UUID
    /// * `request` - Disk resize request with new size
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

// =============================================================================
// Error types for custom methods
// =============================================================================

/// Error type for the get_machine method
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

// =============================================================================
// Serialization helper
// =============================================================================

/// Serialize a request type to JSON Value, panicking on failure.
///
/// All request types in this crate are simple structs (String, Option, Uuid
/// fields) whose serialization cannot fail. This replaces the previous
/// `unwrap_or_default()` pattern which silently produced `Value::Null` on
/// error, masking bugs as confusing server-side failures.
#[allow(clippy::expect_used)]
fn to_json_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("request serialization should not fail")
}

/// Wrapper that adds an `action` field to a request body for action-dispatch
/// endpoints. Node.js CloudAPI expects action in the body: `{"action": "stop"}`.
#[derive(serde::Serialize)]
struct ActionBody<A: serde::Serialize, B: serde::Serialize> {
    action: A,
    #[serde(flatten)]
    body: B,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Validate that the shared emit-payload fixture file deserializes into the
    /// expected Rust types. This catches drift between the fixture JSON and the
    /// API type definitions at `cargo test` time.
    #[test]
    fn emit_payload_fixture_matches_rust_types() {
        let fixture_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../cli/triton-cli/tests/comparison/fixtures/emit-payload-fakes.json"
        );
        // arch-lint: allow(no-sync-io) reason="Synchronous test function"
        let data = std::fs::read_to_string(fixture_path).expect("fixture file should be readable");
        let fixture: serde_json::Value =
            serde_json::from_str(&data).expect("fixture should be valid JSON");
        let responses = fixture
            .get("responses")
            .expect("fixture should have 'responses' key");

        // Concrete values for placeholder interpolation
        let test_uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let test_seg = "test-name";
        let test_vlan_id = 42;

        // Helper: interpolate a fixture template with concrete values
        let interp = |v: &serde_json::Value| -> String {
            let s = serde_json::to_string(v).unwrap();
            let s = s.replace("$LAST_SEG_UUID", test_uuid);
            let s = s.replace("$LAST_SEG", test_seg);
            s.replace("\"$VLAN_ID\"", &test_vlan_id.to_string())
        };

        // Each (operation_id, type) pair to validate
        let image_json = interp(&responses["get_image"]);
        serde_json::from_str::<Image>(&image_json)
            .expect("get_image fixture should deserialize as Image");

        let machine_json = interp(&responses["get_machine"]);
        serde_json::from_str::<Machine>(&machine_json)
            .expect("get_machine fixture should deserialize as Machine");

        let package_json = interp(&responses["get_package"]);
        serde_json::from_str::<Package>(&package_json)
            .expect("get_package fixture should deserialize as Package");

        let network_json = interp(&responses["get_network"]);
        serde_json::from_str::<Network>(&network_json)
            .expect("get_network fixture should deserialize as Network");

        let fabric_network_json = interp(&responses["get_fabric_network"]);
        serde_json::from_str::<Network>(&fabric_network_json)
            .expect("get_fabric_network fixture should deserialize as Network");

        let fwrule_json = interp(&responses["get_firewall_rule"]);
        serde_json::from_str::<FirewallRule>(&fwrule_json)
            .expect("get_firewall_rule fixture should deserialize as FirewallRule");

        let volume_json = interp(&responses["get_volume"]);
        serde_json::from_str::<Volume>(&volume_json)
            .expect("get_volume fixture should deserialize as Volume");

        let key_json = interp(&responses["get_key"]);
        serde_json::from_str::<SshKey>(&key_json)
            .expect("get_key fixture should deserialize as SshKey");

        let nic_json = interp(&responses["get_nic"]);
        serde_json::from_str::<Nic>(&nic_json).expect("get_nic fixture should deserialize as Nic");

        let snapshot_json = interp(&responses["get_machine_snapshot"]);
        serde_json::from_str::<Snapshot>(&snapshot_json)
            .expect("get_machine_snapshot fixture should deserialize as Snapshot");

        let disk_json = interp(&responses["get_machine_disk"]);
        serde_json::from_str::<Disk>(&disk_json)
            .expect("get_machine_disk fixture should deserialize as Disk");

        let vlan_json = interp(&responses["get_fabric_vlan"]);
        serde_json::from_str::<FabricVlan>(&vlan_json)
            .expect("get_fabric_vlan fixture should deserialize as FabricVlan");

        let update_vlan_json = interp(&responses["update_fabric_vlan"]);
        serde_json::from_str::<FabricVlan>(&update_vlan_json)
            .expect("update_fabric_vlan fixture should deserialize as FabricVlan");
    }
}
