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
//!     "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99",
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
//! ### Unauthenticated Client
//!
//! For unauthenticated requests (limited API access):
//!
//! ```ignore
//! use cloudapi_client::UnauthenticatedClient;
//!
//! let client = UnauthenticatedClient::new("https://cloudapi.example.com");
//! // Only works for endpoints that don't require authentication
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
//!     "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99",
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

// Include the Progenitor-generated client code
// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/client.rs"));
}
pub use generated::*;

// Re-export triton-auth types for convenience
pub use triton_auth::{AuthConfig, AuthError, KeySource};

// Re-export types from the API crate for convenience
pub use cloudapi_api::{
    // Key types
    AccessKey,
    AccessKeyPath,
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
    Datacenter,
    Datacenters,
    DisableDeletionProtectionRequest,
    DisableFirewallRequest,
    Disk,
    DiskAction,
    DiskActionQuery,
    DiskPath,
    DiskSpec,
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

    // ========================================================================
    // Machine Creation (with legacy format transformation)
    // ========================================================================

    /// Create a machine with legacy-compatible format
    ///
    /// This method transforms the clean `CreateMachineRequest` format into the
    /// legacy dot-notation format expected by the Node.js CloudAPI server.
    ///
    /// Node.js CloudAPI expects tags as `tag.KEY=VALUE` and metadata as
    /// `metadata.KEY=VALUE` as top-level JSON fields, not nested objects.
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
        request: &CreateMachineRequest,
    ) -> Result<types::Machine, CreateMachineError> {
        // Transform the request to legacy format with flattened tags/metadata
        let legacy_body = transform_create_machine_to_legacy(request);

        // Build the URL and path for signing
        // Note: base_url might have trailing slash, so trim it
        let base = self.base_url.trim_end_matches('/');
        let path = format!("/{}/machines", account);
        let url = format!("{}{}", base, path);

        // Sign the request using triton-auth
        let (date_header, auth_header) =
            triton_auth::sign_request(&self.auth_config, "POST", &path)
                .await
                .map_err(CreateMachineError::Auth)?;

        // Send the request with our transformed body
        let response = self
            .http_client
            .post(&url)
            .header("Date", &date_header)
            .header("Authorization", &auth_header)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
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

    /// Create a machine from the Progenitor-generated request type
    ///
    /// This method accepts the Progenitor-generated `types::CreateMachineRequest` and
    /// transforms it to the legacy format expected by Node.js CloudAPI.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `request` - Machine creation request (Progenitor type)
    ///
    /// # Errors
    /// Returns an error if the request fails or the server returns an error.
    pub async fn create_machine_from_progenitor(
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
        let url = format!("{}{}", base, path);

        // Sign the request using triton-auth
        let (date_header, auth_header) =
            triton_auth::sign_request(&self.auth_config, "POST", &path)
                .await
                .map_err(CreateMachineError::Auth)?;

        // Send the request with our transformed body
        let response = self
            .http_client
            .post(&url)
            .header("Date", &date_header)
            .header("Authorization", &auth_header)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
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
    /// This method handles the CloudAPI quirk where deleted machines return
    /// HTTP 410 Gone with the Machine body (instead of an Error body).
    /// The progenitor-generated client fails to parse this, so we handle it
    /// manually here.
    ///
    /// # Arguments
    /// * `account` - Account login name
    /// * `machine` - Machine UUID
    ///
    /// # Returns
    /// Returns the Machine. If the machine is deleted, `machine.state` will be
    /// `MachineState::Deleted`. Check the state to determine if the machine
    /// is still active.
    ///
    /// # Errors
    /// Returns an error if the machine is not found (404) or on other failures.
    pub async fn get_machine(
        &self,
        account: &str,
        machine: &Uuid,
    ) -> Result<types::Machine, GetMachineError> {
        // Build the URL and path for signing
        let base = self.base_url.trim_end_matches('/');
        let path = format!("/{}/machines/{}", account, machine);
        let url = format!("{}{}", base, path);

        // Sign the request using triton-auth
        let (date_header, auth_header) = triton_auth::sign_request(&self.auth_config, "GET", &path)
            .await
            .map_err(GetMachineError::Auth)?;

        // Send the request
        let response = self
            .http_client
            .get(&url)
            .header("Date", &date_header)
            .header("Authorization", &auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(GetMachineError::Request)?;

        let status = response.status();

        // Handle success cases: 200 OK and 410 Gone both return Machine body
        if status.is_success() || status == reqwest::StatusCode::GONE {
            let response_text = response
                .text()
                .await
                .map_err(GetMachineError::ResponseParse)?;

            let machine: types::Machine =
                serde_json::from_str(&response_text).map_err(|e| GetMachineError::JsonParse {
                    error: e.to_string(),
                    body: response_text.clone(),
                })?;

            Ok(machine)
        } else if status == reqwest::StatusCode::NOT_FOUND {
            Err(GetMachineError::NotFound)
        } else {
            let error_body = response.text().await.unwrap_or_default();
            Err(GetMachineError::Server {
                status,
                body: error_body,
            })
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(request).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(&body).unwrap_or_default())
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
            .body(serde_json::to_value(request).unwrap_or_default())
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
            .body(serde_json::to_value(request).unwrap_or_default())
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
    /// Machine not found (404)
    #[error("machine not found")]
    NotFound,
    /// Server returned an error response
    #[error("server error ({status}): {body}")]
    Server {
        status: reqwest::StatusCode,
        body: String,
    },
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
    let mut obj = serde_json::to_value(request).unwrap_or_default();

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

/// Transform a CreateMachineRequest (cloudapi_api type) into the legacy format
///
/// Node.js CloudAPI expects tags as `tag.KEY=VALUE` and metadata as
/// `metadata.KEY=VALUE` as top-level JSON fields, not nested objects.
fn transform_create_machine_to_legacy(request: &CreateMachineRequest) -> serde_json::Value {
    // Start with the standard serialization
    let mut obj = serde_json::json!({
        "image": request.image,
        "package": request.package,
    });

    // SAFETY: obj is created as a JSON object above, so this unwrap is safe
    let Some(map) = obj.as_object_mut() else {
        return obj;
    };

    // Add optional fields
    if let Some(name) = &request.name {
        map.insert("name".to_string(), serde_json::Value::String(name.clone()));
    }

    if let Some(networks) = &request.networks {
        map.insert(
            "networks".to_string(),
            serde_json::to_value(networks).unwrap_or_default(),
        );
    }

    if let Some(nics) = &request.nics {
        map.insert(
            "nics".to_string(),
            serde_json::to_value(nics).unwrap_or_default(),
        );
    }

    if let Some(affinity) = &request.affinity {
        map.insert(
            "affinity".to_string(),
            serde_json::to_value(affinity).unwrap_or_default(),
        );
    }

    if let Some(locality) = &request.locality {
        map.insert("locality".to_string(), locality.clone());
    }

    if let Some(firewall_enabled) = request.firewall_enabled {
        map.insert(
            "firewall_enabled".to_string(),
            serde_json::Value::Bool(firewall_enabled),
        );
    }

    if let Some(deletion_protection) = request.deletion_protection {
        map.insert(
            "deletion_protection".to_string(),
            serde_json::Value::Bool(deletion_protection),
        );
    }

    if let Some(brand) = &request.brand {
        map.insert(
            "brand".to_string(),
            serde_json::to_value(brand).unwrap_or_default(),
        );
    }

    if let Some(volumes) = &request.volumes {
        map.insert(
            "volumes".to_string(),
            serde_json::to_value(volumes).unwrap_or_default(),
        );
    }

    if let Some(disks) = &request.disks {
        map.insert(
            "disks".to_string(),
            serde_json::to_value(disks).unwrap_or_default(),
        );
    }

    if let Some(delegate_dataset) = request.delegate_dataset {
        map.insert(
            "delegate_dataset".to_string(),
            serde_json::Value::Bool(delegate_dataset),
        );
    }

    if let Some(encrypted) = request.encrypted {
        map.insert("encrypted".to_string(), serde_json::Value::Bool(encrypted));
    }

    if let Some(allow_shared_images) = request.allow_shared_images {
        map.insert(
            "allow_shared_images".to_string(),
            serde_json::Value::Bool(allow_shared_images),
        );
    }

    // Flatten tags using the helper method that handles both formats
    let tags = request.tags();
    for (key, value) in tags {
        map.insert(format!("tag.{key}"), value);
    }

    // Flatten metadata using the helper method that handles both formats
    // arch-lint: allow(no-sync-io) reason="metadata() is a struct method, not filesystem I/O"
    let metadata = request.metadata();
    for (key, value) in metadata {
        map.insert(format!("metadata.{key}"), serde_json::Value::String(value));
    }

    obj
}
