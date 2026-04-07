// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IMGAPI Client Library
//!
//! This client provides typed access to the Triton IMGAPI service
//! (Image Management API).
//!
//! ## Usage
//!
//! ### Basic Client
//!
//! For direct API access (internal Triton services):
//!
//! ```ignore
//! use imgapi_client::Client;
//!
//! let client = Client::new("http://imgapi.my-dc.my-cloud.local");
//!
//! // List images
//! let images = client.list_images().send().await?;
//!
//! // Get a specific image
//! let image = client.get_image().uuid(image_uuid).send().await?;
//! ```
//!
//! ### TypedClient for Action-based Endpoints
//!
//! For image action endpoints, use the typed wrapper methods for better
//! ergonomics:
//!
//! ```ignore
//! use imgapi_client::TypedClient;
//!
//! let client = TypedClient::new("http://imgapi.my-dc.my-cloud.local");
//!
//! // Activate an image
//! client.activate_image(&image_uuid).await?;
//!
//! // Update image fields
//! let update = imgapi_api::UpdateImageRequest {
//!     name: Some("new-name".to_string()),
//!     ..Default::default()
//! };
//! client.update_image(&image_uuid, &update).await?;
//!
//! // Access underlying client for other operations
//! let channels = client.inner().list_channels().send().await?;
//! ```

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

// Re-export types from the API crate for convenience.
pub use imgapi_api::{
    // Common types
    AccountQuery,
    // Action enums
    AclAction,
    AclActionQuery,
    // Per-action request types (POST /images/:uuid)
    ActivateImageRequest,
    // File/icon types
    AddImageFileFromUrlQuery,
    AddImageFileFromUrlRequest,
    AddImageFileQuery,
    AddImageIconQuery,
    // Push/admin
    AdminPushQuery,
    ChangeStorQuery,
    // Image types
    Channel,
    ChannelAddImageRequest,
    ChannelQuery,
    CloneImageQuery,
    CreateImageAction,
    CreateImageActionQuery,
    // Create action types (POST /images)
    CreateImageFromVmRequest,
    CreateImageRequest,
    DatasetPath,
    DeleteImageIconQuery,
    DeleteImageQuery,
    DisableImageRequest,
    EnableImageRequest,
    ExportImageQuery,
    ExportImageResponse,
    FileCompression,
    GetImageFileQuery,
    GetImageIconQuery,
    Image,
    ImageAction,
    ImageActionQuery,
    ImageError,
    ImageFile,
    ImageOs,
    ImagePath,
    ImageRequirements,
    ImageState,
    ImageType,
    ImageUser,
    ImportDockerImageQuery,
    ImportFromDatacenterQuery,
    ImportImageFromSourceQuery,
    ImportImageRequest,
    ImportLxdImageQuery,
    ImportRemoteImageQuery,
    JobResponse,
    ListImageJobsQuery,
    ListImagesQuery,
    NetworkRequirement,
    // Ping
    PingQuery,
    PingResponse,
    StateAction,
    StateActionQuery,
    StorageType,
    UpdateImageRequest,
    // Common
    Uuid,
};

use futures_util::TryStreamExt;

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

/// Wraps an action enum + per-action request body into a single JSON object.
/// Produces `{"action": "<variant>", ...body_fields}` via `#[serde(flatten)]`.
///
/// Node.js Restify's `mapParams: true` merges query and body params, so
/// clients may send `action` in either place. We send it in the body to
/// match the established wire format.
#[derive(serde::Serialize)]
struct ActionBody<A: serde::Serialize, B: serde::Serialize> {
    action: A,
    #[serde(flatten)]
    body: B,
}

/// Collect a `ByteStream` into a `Vec<u8>`.
///
/// Used by typed wrappers to parse JSON from `Response<Body>` endpoints.
async fn collect_byte_stream(stream: ByteStream) -> Result<Vec<u8>, ActionError> {
    let chunks: Vec<bytes::Bytes> = stream
        .into_inner()
        .try_collect()
        .await
        .map_err(ActionError::Reqwest)?;
    let mut buf = Vec::new();
    for chunk in chunks {
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Typed client wrapper for action-based endpoints
///
/// This wrapper provides ergonomic methods for IMGAPI's action-based endpoints
/// (image actions, create actions, ACL actions) while still allowing access to
/// the underlying Progenitor-generated client for all other operations.
///
/// Action dispatch endpoints (`create_image`, `image_action`) return
/// `ByteStream` from Progenitor because the underlying Dropshot trait uses
/// `Response<Body>` (multiple response shapes per action). The typed methods
/// here parse the response body into the correct type for each action.
pub struct TypedClient {
    inner: Client,
}

impl TypedClient {
    /// Create a new typed client wrapper
    ///
    /// # Arguments
    /// * `base_url` - IMGAPI base URL (e.g., "http://imgapi.my-dc.my-cloud.local")
    pub fn new(base_url: &str) -> Self {
        Self {
            inner: Client::new(base_url),
        }
    }

    /// Create a typed client with a custom reqwest client
    ///
    /// # Arguments
    /// * `base_url` - IMGAPI base URL
    /// * `http_client` - Custom reqwest client
    pub fn new_with_client(base_url: &str, http_client: reqwest::Client) -> Self {
        Self {
            inner: Client::new_with_client(base_url, http_client),
        }
    }

    /// Access the underlying Progenitor client for non-wrapped methods
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    // ========================================================================
    // POST /images/:uuid actions (ImageAction)
    // ========================================================================

    /// Import an image manifest (admin-only)
    ///
    /// # Arguments
    /// * `uuid` - Image UUID
    /// * `request` - Full image manifest to import
    pub async fn import_image(
        &self,
        uuid: &Uuid,
        request: &ImportImageRequest,
    ) -> Result<Image, ActionError> {
        self.image_action_json(uuid, types::ImageAction::Import, request)
            .await
    }

    /// Import an image from a remote IMGAPI (admin-only, creates workflow job)
    ///
    /// The real Node.js IMGAPI expects `source` and `skip_owner_check` as
    /// query parameters, not body fields.
    pub async fn import_remote_image(
        &self,
        uuid: &Uuid,
        source: &str,
        skip_owner_check: bool,
    ) -> Result<JobResponse, ActionError> {
        let mut req = self
            .inner
            .image_action()
            .uuid(*uuid)
            .action(types::ImageAction::ImportRemote)
            .source(source)
            .body(serde_json::json!({}));
        if skip_owner_check {
            req = req.skip_owner_check(true);
        }
        let resp = req.send().await.map_err(ActionError::ByteStream)?;
        let bytes = collect_byte_stream(resp.into_inner()).await?;
        serde_json::from_slice(&bytes).map_err(ActionError::Deserialize)
    }

    /// Import an image from another datacenter (creates workflow job)
    pub async fn import_from_datacenter(
        &self,
        uuid: &Uuid,
        datacenter: &str,
        account: &Uuid,
    ) -> Result<JobResponse, ActionError> {
        self.image_action_json(
            uuid,
            types::ImageAction::ImportFromDatacenter,
            &serde_json::json!({
                "datacenter": datacenter,
                "account": account,
            }),
        )
        .await
    }

    /// Change storage backend for an image (admin-only)
    pub async fn change_stor(&self, uuid: &Uuid, stor: StorageType) -> Result<Image, ActionError> {
        self.image_action_json(
            uuid,
            types::ImageAction::ChangeStor,
            &serde_json::json!({ "stor": stor }),
        )
        .await
    }

    /// Export an image to Manta
    pub async fn export_image(
        &self,
        uuid: &Uuid,
        manta_path: &str,
        account: Option<&Uuid>,
    ) -> Result<ExportImageResponse, ActionError> {
        self.image_action_json(
            uuid,
            types::ImageAction::Export,
            &serde_json::json!({
                "manta_path": manta_path,
                "account": account,
            }),
        )
        .await
    }

    /// Activate an unactivated image
    pub async fn activate_image(&self, uuid: &Uuid) -> Result<Image, ActionError> {
        self.image_action_json(uuid, types::ImageAction::Activate, &ActivateImageRequest {})
            .await
    }

    /// Enable a disabled image
    pub async fn enable_image(&self, uuid: &Uuid) -> Result<Image, ActionError> {
        self.image_action_json(uuid, types::ImageAction::Enable, &EnableImageRequest {})
            .await
    }

    /// Disable an active image
    pub async fn disable_image(&self, uuid: &Uuid) -> Result<Image, ActionError> {
        self.image_action_json(uuid, types::ImageAction::Disable, &DisableImageRequest {})
            .await
    }

    /// Add an image to a channel
    pub async fn channel_add_image(
        &self,
        uuid: &Uuid,
        channel: &str,
    ) -> Result<Image, ActionError> {
        self.image_action_json(
            uuid,
            types::ImageAction::ChannelAdd,
            &ChannelAddImageRequest {
                channel: channel.to_string(),
            },
        )
        .await
    }

    /// Update mutable image fields
    pub async fn update_image(
        &self,
        uuid: &Uuid,
        request: &UpdateImageRequest,
    ) -> Result<Image, ActionError> {
        self.image_action_json(uuid, types::ImageAction::Update, request)
            .await
    }

    // Note: import-docker-image, import-lxd-image are streaming responses
    // and cannot be deserialized to a single typed response. Use the raw
    // `inner().image_action()` builder for these.

    // ========================================================================
    // POST /images actions (CreateImageAction)
    // ========================================================================

    /// Create an image from a manifest
    ///
    /// # Arguments
    /// * `request` - Image manifest to create
    /// * `channel` - Optional channel name
    pub async fn create_image_from_manifest(
        &self,
        request: &CreateImageRequest,
        channel: Option<&str>,
    ) -> Result<Image, ActionError> {
        let body = to_json_value(request);
        let mut builder = self.inner.create_image().body(body);
        if let Some(ch) = channel {
            builder = builder.channel(ch);
        }
        let resp = builder.send().await.map_err(ActionError::ByteStream)?;
        let bytes = collect_byte_stream(resp.into_inner()).await?;
        serde_json::from_slice(&bytes).map_err(ActionError::Deserialize)
    }

    /// Create an image from an existing VM (creates workflow job)
    ///
    /// # Arguments
    /// * `request` - Create-from-VM request with VM UUID and image details
    /// * `channel` - Optional channel name
    /// * `account` - Optional account UUID
    pub async fn create_image_from_vm(
        &self,
        request: &CreateImageFromVmRequest,
        channel: Option<&str>,
        account: Option<&Uuid>,
    ) -> Result<JobResponse, ActionError> {
        let body = ActionBody {
            action: CreateImageAction::CreateFromVm,
            body: request,
        };
        let mut builder = self
            .inner
            .create_image()
            .action(types::CreateImageAction::CreateFromVm)
            .body(to_json_value(&body));
        if let Some(ch) = channel {
            builder = builder.channel(ch);
        }
        if let Some(acct) = account {
            builder = builder.account(*acct);
        }
        let resp = builder.send().await.map_err(ActionError::ByteStream)?;
        let bytes = collect_byte_stream(resp.into_inner()).await?;
        serde_json::from_slice(&bytes).map_err(ActionError::Deserialize)
    }

    // Note: import-docker-image, import-lxd-image, import-from-datacenter
    // via POST /images are streaming or special responses. Use the raw
    // `inner().create_image()` builder for these.

    // ========================================================================
    // POST /images/:uuid/acl actions (AclAction)
    // ========================================================================

    /// Add account UUIDs to an image's ACL
    ///
    /// # Arguments
    /// * `uuid` - Image UUID
    /// * `acl_uuids` - Account UUIDs to grant access
    pub async fn add_image_acl(
        &self,
        uuid: &Uuid,
        acl_uuids: &[Uuid],
    ) -> Result<types::Image, ActionError> {
        self.inner
            .image_acl_action()
            .uuid(*uuid)
            .action(types::AclAction::Add)
            .body(acl_uuids.to_vec())
            .send()
            .await
            .map(|r| r.into_inner())
            .map_err(ActionError::Typed)
    }

    /// Remove account UUIDs from an image's ACL
    ///
    /// # Arguments
    /// * `uuid` - Image UUID
    /// * `acl_uuids` - Account UUIDs to revoke access
    pub async fn remove_image_acl(
        &self,
        uuid: &Uuid,
        acl_uuids: &[Uuid],
    ) -> Result<types::Image, ActionError> {
        self.inner
            .image_acl_action()
            .uuid(*uuid)
            .action(types::AclAction::Remove)
            .body(acl_uuids.to_vec())
            .send()
            .await
            .map(|r| r.into_inner())
            .map_err(ActionError::Typed)
    }

    // ========================================================================
    // POST /state actions (StateAction)
    // ========================================================================

    /// Drop all internal caches (admin-only)
    pub async fn drop_caches(&self) -> Result<(), ActionError> {
        self.inner
            .admin_update_state()
            .action(types::StateAction::Dropcaches)
            .send()
            .await
            .map(|_| ())
            .map_err(ActionError::Typed)
    }

    // ========================================================================
    // Helper methods
    // ========================================================================

    /// Send an image action and parse the JSON response.
    ///
    /// The action is sent as a query parameter (matching the real Node.js
    /// IMGAPI's expected wire format) and the body contains only the
    /// action-specific fields.
    async fn image_action_json<T>(
        &self,
        uuid: &Uuid,
        action: types::ImageAction,
        body: &impl serde::Serialize,
    ) -> Result<T, ActionError>
    where
        T: serde::de::DeserializeOwned,
    {
        let resp = self
            .inner
            .image_action()
            .uuid(*uuid)
            .action(action)
            .body(to_json_value(body))
            .send()
            .await
            .map_err(ActionError::ByteStream)?;
        let bytes = collect_byte_stream(resp.into_inner()).await?;
        serde_json::from_slice(&bytes).map_err(ActionError::Deserialize)
    }
}

/// Error type for typed client wrapper methods
///
/// Wraps both Progenitor errors (for endpoints that return typed errors)
/// and ByteStream errors (for endpoints that return `Response<Body>`).
#[derive(Debug)]
pub enum ActionError {
    /// Error from a typed endpoint (returns `types::Error`)
    Typed(Error<types::Error>),
    /// Error from a ByteStream endpoint (returns raw bytes)
    ByteStream(Error<ByteStream>),
    /// Error from reqwest when reading the response stream
    Reqwest(reqwest::Error),
    /// Error deserializing the response body
    Deserialize(serde_json::Error),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Typed(e) => write!(f, "{e}"),
            ActionError::ByteStream(e) => write!(f, "{e}"),
            ActionError::Reqwest(e) => write!(f, "request error: {e}"),
            ActionError::Deserialize(e) => write!(f, "response deserialization failed: {e}"),
        }
    }
}

impl std::error::Error for ActionError {}
