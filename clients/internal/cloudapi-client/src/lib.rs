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

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

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
    MigrationEstimateRequest,
    MigrationPhase,
    MigrationState,
    Network,
    NetworkIp,
    NetworkIpPath,
    NetworkPath,
    Nic,
    NicPath,
    NicSpec,
    NicState,
    Package,
    PackagePath,
    Policy,
    PolicyPath,
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
    ShareImageRequest,
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
    UnshareImageRequest,
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
    VolumeState,
};

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
    pub brand: Option<types::Brand>,
    /// Filter by docker flag
    pub docker: Option<bool>,
    /// Pagination offset
    pub offset: Option<u64>,
    /// Pagination limit
    pub limit: Option<u64>,
    /// Filter by tag (key, value)
    pub tag: Option<(String, String)>,
}

/// Typed client wrapper for action-based endpoints
///
/// This wrapper provides ergonomic methods for CloudAPI's action-based endpoints
/// (machines, images, volumes, disks) while still allowing access to the underlying
/// Progenitor-generated client for all other operations.
///
/// This client is authenticated and will automatically sign all requests.
pub struct TypedClient {
    inner: Client,
    auth_config: AuthConfig,
    http_client: reqwest::Client,
    base_url: String,
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
            base_url: base_url.to_string(),
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
            base_url: base_url.to_string(),
        })
    }

    /// Access the underlying Progenitor client for non-wrapped methods
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    /// Get the authentication configuration
    pub fn auth_config(&self) -> &AuthConfig {
        &self.auth_config
    }

    /// Access the underlying reqwest HTTP client
    ///
    /// This shares the same TLS configuration (e.g., insecure mode)
    /// as the Progenitor-generated client.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    // ========================================================================
    // Machine Creation (with legacy format transformation)
    // ========================================================================

    /// Create a machine with legacy-compatible format
    ///
    /// This method accepts the Progenitor-generated `types::CreateMachineRequest` and
    /// transforms it to the legacy format expected by Node.js CloudAPI.
    ///
    /// Node.js CloudAPI expects tags as `tag.KEY=VALUE` and metadata as
    /// `metadata.KEY=VALUE` as top-level JSON fields, not nested objects.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `request` - Machine creation request (Progenitor type)
    ///
    /// # Errors
    /// Returns an error if the request fails or the server returns an error.
    pub async fn create_machine(
        &self,
        account: &str,
        request: &types::CreateMachineRequest,
    ) -> Result<types::Machine, CreateMachineError> {
        // Transform via JSON serialization and legacy flattening
        let legacy_body = transform_progenitor_request_to_legacy(request);

        // Build the URL and path for signing
        // Note: base_url might have trailing slash, so trim it
        let base = self.base_url.trim_end_matches('/');
        let path = format!("/{}/machines", account);

        // Add RBAC roles as query parameter before signing so the
        // signature covers the role parameter
        let path_and_query = if let Some(roles) = &self.auth_config.roles
            && !roles.is_empty()
        {
            let encoded_roles: Vec<String> = roles
                .iter()
                .map(|r| urlencoding::encode(r).into_owned())
                .collect();
            format!("{}?as-role={}", path, encoded_roles.join(","))
        } else {
            path.clone()
        };

        let url = format!("{}{}", base, path_and_query);

        // Sign the request using triton-auth (path includes role query
        // param so the signature covers it)
        let (date_header, auth_header) =
            triton_auth::sign_request(&self.auth_config, "POST", &path_and_query)
                .await
                .map_err(CreateMachineError::Auth)?;

        // Send the request with our transformed body
        let mut req = self
            .http_client
            .post(&url)
            .header("Date", &date_header)
            .header("Authorization", &auth_header)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");

        // Add X-Act-As header if present (for operator masquerading)
        if let Some(act_as) = &self.auth_config.act_as {
            req = req.header("x-act-as", act_as);
        }

        // Add Accept-Version header if present (for API versioning)
        if let Some(version) = &self.auth_config.accept_version {
            req = req.header("accept-version", version);
        }

        let response = req
            .json(&legacy_body)
            .send()
            .await
            .map_err(CreateMachineError::Request)?;

        let status = response.status();
        if status.is_success() {
            // Get the response text first for debugging
            let response_text = response
                .text()
                .await
                .map_err(CreateMachineError::ResponseParse)?;
            // Try to parse as Machine
            let machine: types::Machine = serde_json::from_str(&response_text).map_err(|e| {
                CreateMachineError::JsonParse {
                    error: e.to_string(),
                    body: response_text.clone(),
                }
            })?;
            Ok(machine)
        } else {
            let error_body = response.text().await.unwrap_or_default();
            Err(CreateMachineError::Server {
                status,
                body: error_body,
            })
        }
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
            // with the raw bytes still intact.
            Err(Error::InvalidResponsePayload(bytes, _)) => {
                serde_json::from_slice(&bytes).map_err(|e| GetMachineError::JsonParse {
                    error: e.to_string(),
                    body: String::from_utf8_lossy(&bytes).to_string(),
                })
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
        let body = StartMachineRequest { origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::Start)
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
        let body = StopMachineRequest { origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::Stop)
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
        let body = RebootMachineRequest { origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::Reboot)
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
        let body = ResizeMachineRequest { package, origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::Resize)
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
        let body = RenameMachineRequest { name, origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::Rename)
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
        let body = EnableFirewallRequest { origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::EnableFirewall)
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
        let body = DisableFirewallRequest { origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::DisableFirewall)
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
        let body = EnableDeletionProtectionRequest { origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::EnableDeletionProtection)
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
        let body = DisableDeletionProtectionRequest { origin };
        self.inner
            .update_machine()
            .account(account)
            .machine(machine.to_string())
            .action(types::MachineAction::DisableDeletionProtection)
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
        let body = ExportImageRequest { manta_path };
        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::Export)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Clone image to account
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `dataset` - Image UUID
    pub async fn clone_image(
        &self,
        account: &str,
        dataset: &Uuid,
    ) -> Result<types::Image, Error<types::Error>> {
        let body = CloneImageRequest {};
        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::Clone)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Import image from another datacenter
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `dataset` - Image UUID (in current datacenter)
    /// * `datacenter` - Source datacenter name
    /// * `id` - Image UUID in source datacenter
    pub async fn import_image_from_datacenter(
        &self,
        account: &str,
        dataset: &Uuid,
        datacenter: String,
        id: Uuid,
    ) -> Result<types::Image, Error<types::Error>> {
        let body = ImportImageRequest { datacenter, id };
        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::ImportFromDatacenter)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Share image with another account
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
        let body = ShareImageRequest {
            account: target_account,
        };
        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::Share)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Unshare image from another account
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
        let body = UnshareImageRequest {
            account: target_account,
        };
        self.inner
            .update_image()
            .account(account)
            .dataset(dataset.to_string())
            .action(types::ImageAction::Unshare)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    // ========================================================================
    // Volume Actions
    // ========================================================================

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
        self.inner
            .update_volume()
            .account(account)
            .id(id.to_string())
            .action(types::VolumeAction::Update)
            .body(to_json_value(request))
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
        self.inner
            .resize_machine_disk()
            .account(account)
            .machine(machine.to_string())
            .disk(disk.to_string())
            .action(types::DiskAction::Resize)
            .body(to_json_value(request))
            .send()
            .await
            .map(|r| r.into_inner())
    }
}

// =============================================================================
// Error types for custom methods
// =============================================================================

/// Error type for the create_machine method
#[derive(Debug, thiserror::Error)]
pub enum CreateMachineError {
    /// Authentication/signing error
    #[error("authentication error: {0}")]
    Auth(#[from] AuthError),
    /// HTTP request error
    #[error("request error: {0}")]
    Request(reqwest::Error),
    /// Failed to parse response body
    #[error("failed to parse response: {0}")]
    ResponseParse(reqwest::Error),
    /// Failed to parse response JSON
    #[error("failed to parse JSON: {error}\nResponse body: {body}")]
    JsonParse { error: String, body: String },
    /// Server returned an error response
    #[error("server error ({status}): {body}")]
    Server {
        status: reqwest::StatusCode,
        body: String,
    },
}

/// Error type for the get_machine method
#[derive(Debug, thiserror::Error)]
pub enum GetMachineError {
    /// Progenitor client error (auth, transport, server errors)
    #[error("{0}")]
    Client(String),
    /// Failed to parse response JSON (usually a 410 Gone body)
    #[error("failed to parse JSON: {error}\nResponse body: {body}")]
    JsonParse { error: String, body: String },
    /// Machine not found (404)
    #[error("machine not found")]
    NotFound,
}

// =============================================================================
// Infallible serialization helper
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

// =============================================================================
// Legacy format transformation for Node.js CloudAPI compatibility
// =============================================================================

/// Transform a Progenitor-generated CreateMachineRequest into the legacy format
///
/// This handles the Progenitor-generated type which uses serde_json::Map for tags/metadata.
fn transform_progenitor_request_to_legacy(
    request: &types::CreateMachineRequest,
) -> serde_json::Value {
    // Serialize the request to JSON, then extract and flatten tags/metadata
    let mut obj = to_json_value(request);

    if let Some(map) = obj.as_object_mut() {
        // Extract and flatten tags
        if let Some(tags) = map.remove("tags")
            && let Some(tags_obj) = tags.as_object()
        {
            for (key, value) in tags_obj {
                map.insert(format!("tag.{key}"), value.clone());
            }
        }

        // Extract and flatten metadata
        if let Some(metadata) = map.remove("metadata")
            && let Some(meta_obj) = metadata.as_object()
        {
            for (key, value) in meta_obj {
                // Skip password fields (handled separately by CloudAPI)
                if !key.ends_with("_pw") {
                    map.insert(format!("metadata.{key}"), value.clone());
                }
            }
        }
    }

    obj
}
