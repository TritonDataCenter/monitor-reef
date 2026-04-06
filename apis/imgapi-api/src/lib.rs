// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IMGAPI (Image API) trait definition
//!
//! This crate defines the API trait for Triton's IMGAPI service (version 4.13.1).
//! IMGAPI manages virtual machine images in a Triton datacenter.
//!
//! # API Overview
//!
//! IMGAPI provides the following functionality:
//! - Image lifecycle management (create, activate, disable, delete)
//! - Image file upload/download (binary)
//! - Image icon upload/download (binary)
//! - Image ACL management
//! - Image import (from manifest, remote IMGAPI, datacenter, Docker, LXD)
//! - Image export (to Manta)
//! - Image cloning
//! - Channel management
//! - Admin operations (state, auth key reload)
//!
//! # JSON Field Naming
//!
//! IMGAPI uses snake_case for all JSON field names (standard for Triton internal
//! APIs). The one exception is `uncompressedDigest` on `ImageFile` which is camelCase.
//!
//! # Action Dispatch
//!
//! POST /images/:uuid is an action-dispatch endpoint supporting 12 different
//! actions. POST /images also dispatches based on action (create-from-vm, etc.).
//! The API trait models each action dispatch as a single endpoint with
//! `TypedBody<serde_json::Value>` for the body, with typed request structs
//! exported for client use.
//!
//! # Binary Endpoints
//!
//! File and icon upload/download use `UntypedBody` for uploads and
//! `Response<Body>` for downloads, as these handle raw binary data.
//!
//! # Streaming Endpoints
//!
//! Docker/LXD import and push endpoints return streaming JSON progress messages.
//! These use `Response<Body>` for the raw streaming response.
//!
//! # Legacy Dataset Endpoints
//!
//! The `/datasets` endpoints are legacy redirects from DSAPI to `/images`.
//! They return HTTP redirects via `Response<Body>`.

use dropshot::{
    Body, HttpError, HttpResponseAccepted, HttpResponseDeleted, HttpResponseOk, Path, Query,
    RequestContext, TypedBody, UntypedBody,
};
use http::Response;

pub mod types;
pub use types::*;

/// IMGAPI trait definition
///
/// This trait defines all endpoints of the Triton IMGAPI service (version 4.13.1).
#[dropshot::api_description]
pub trait ImgApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    // ========================================================================
    // Health / Ping
    // ========================================================================

    /// Ping the IMGAPI service
    ///
    /// Returns a ping response with version information. If `?error=<name>` is
    /// set, triggers an error response for testing purposes.
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["health"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
        query: Query<PingQuery>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    // ========================================================================
    // Admin State
    // ========================================================================

    /// Get admin state snapshot
    ///
    /// Returns the internal state of the IMGAPI service (admin-only).
    #[endpoint {
        method = GET,
        path = "/state",
        tags = ["admin"],
    }]
    async fn admin_get_state(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Update admin state (action dispatch)
    ///
    /// Currently only supports `?action=dropcaches` which drops all internal
    /// caches. Returns 202 Accepted. Admin-only.
    #[endpoint {
        method = POST,
        path = "/state",
        tags = ["admin"],
    }]
    async fn admin_update_state(
        rqctx: RequestContext<Self::Context>,
        query: Query<StateActionQuery>,
    ) -> Result<HttpResponseAccepted<()>, HttpError>;

    // ========================================================================
    // Channels
    // ========================================================================

    /// List channels
    ///
    /// Returns the configured channels. Only available when IMGAPI is configured
    /// with channel support.
    #[endpoint {
        method = GET,
        path = "/channels",
        tags = ["channels"],
    }]
    async fn list_channels(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<Channel>>, HttpError>;

    // ========================================================================
    // Images - Core CRUD
    // ========================================================================

    /// List images
    ///
    /// Returns a filtered list of images.
    #[endpoint {
        method = GET,
        path = "/images",
        tags = ["images"],
    }]
    async fn list_images(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListImagesQuery>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Get a single image
    ///
    /// Returns the image manifest for the specified UUID.
    /// Supports ETag/conditional requests (implemented in service layer).
    #[endpoint {
        method = GET,
        path = "/images/{uuid}",
        tags = ["images"],
    }]
    async fn get_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<AccountQuery>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Create a new image (action dispatch)
    ///
    /// When no `action` query parameter is provided, creates an image from
    /// the manifest in the request body.
    ///
    /// Supported actions:
    /// - (no action): Create image from manifest body
    /// - `create-from-vm`: Create image from an existing VM
    /// - `import-docker-image`: Import a Docker image (streaming, admin-only)
    /// - `import-lxd-image`: Import an LXD image (streaming, admin-only)
    /// - `import-from-datacenter`: Import from another datacenter
    ///
    /// For streaming actions (import-docker-image, import-lxd-image), the
    /// response is newline-delimited JSON progress messages.
    #[endpoint {
        method = POST,
        path = "/images",
        tags = ["images"],
    }]
    async fn create_image(
        rqctx: RequestContext<Self::Context>,
        query: Query<CreateImageActionQuery>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<Response<Body>, HttpError>;

    /// Update/action on an existing image (action dispatch)
    ///
    /// This endpoint dispatches to different handlers based on the `action`
    /// query parameter. The action may also be sent in the request body.
    ///
    /// Supported actions:
    /// - `import`: Import image manifest (admin-only)
    /// - `import-remote`: Import from remote IMGAPI (admin-only, creates job)
    /// - `import-from-datacenter`: Import from datacenter (creates job)
    /// - `import-docker-image`: Import Docker image (streaming, admin-only)
    /// - `import-lxd-image`: Import LXD image (streaming, admin-only)
    /// - `change-stor`: Change storage backend (admin-only)
    /// - `export`: Export to Manta
    /// - `activate`: Activate an unactivated image
    /// - `enable`: Enable a disabled image
    /// - `disable`: Disable an active image
    /// - `channel-add`: Add image to a channel
    /// - `update`: Update mutable image fields
    ///
    /// Response varies by action (Image, JobResponse, ExportImageResponse,
    /// or streaming JSON).
    #[endpoint {
        method = POST,
        path = "/images/{uuid}",
        tags = ["images"],
    }]
    async fn image_action(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<ImageActionQuery>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<Response<Body>, HttpError>;

    /// Delete an image
    ///
    /// Deletes the specified image and its files. Returns 204 No Content.
    #[endpoint {
        method = DELETE,
        path = "/images/{uuid}",
        tags = ["images"],
    }]
    async fn delete_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<DeleteImageQuery>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Images - File Management
    // ========================================================================

    /// Upload an image file
    ///
    /// Uploads a raw binary file for the specified image. If the `source` query
    /// parameter is set, fetches the file from a remote IMGAPI instead of
    /// reading from the request body.
    #[endpoint {
        method = PUT,
        path = "/images/{uuid}/file",
        tags = ["image-files"],
    }]
    async fn add_image_file(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<AddImageFileQuery>,
        body: UntypedBody,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Add an image file from a URL
    ///
    /// Instructs IMGAPI to fetch the image file from the specified URL.
    #[endpoint {
        method = POST,
        path = "/images/{uuid}/file/from-url",
        tags = ["image-files"],
    }]
    async fn add_image_file_from_url(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<AddImageFileFromUrlQuery>,
        body: TypedBody<AddImageFileFromUrlRequest>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Download an image file
    ///
    /// Returns the raw binary content of the image file.
    /// Supports ETag/conditional requests.
    #[endpoint {
        method = GET,
        path = "/images/{uuid}/file",
        tags = ["image-files"],
    }]
    async fn get_image_file(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<GetImageFileQuery>,
    ) -> Result<Response<Body>, HttpError>;

    // ========================================================================
    // Images - Icon Management
    // ========================================================================

    /// Upload an image icon
    ///
    /// Uploads a raw binary icon (image/*) for the specified image.
    #[endpoint {
        method = PUT,
        path = "/images/{uuid}/icon",
        tags = ["image-icons"],
    }]
    async fn add_image_icon(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<AddImageIconQuery>,
        body: UntypedBody,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Download an image icon
    ///
    /// Returns the raw binary content of the image icon.
    #[endpoint {
        method = GET,
        path = "/images/{uuid}/icon",
        tags = ["image-icons"],
    }]
    async fn get_image_icon(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<GetImageIconQuery>,
    ) -> Result<Response<Body>, HttpError>;

    /// Delete an image icon
    ///
    /// Removes the icon from the specified image. Returns the updated image.
    #[endpoint {
        method = DELETE,
        path = "/images/{uuid}/icon",
        tags = ["image-icons"],
    }]
    async fn delete_image_icon(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<DeleteImageIconQuery>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    // ========================================================================
    // Images - ACL Management
    // ========================================================================

    /// Modify image ACL (action dispatch)
    ///
    /// Adds or removes account UUIDs from the image's access control list.
    /// Default action is "add" if not specified.
    ///
    /// Body is a JSON array of account UUIDs.
    #[endpoint {
        method = POST,
        path = "/images/{uuid}/acl",
        tags = ["image-acl"],
    }]
    async fn image_acl_action(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<AclActionQuery>,
        body: TypedBody<Vec<uuid::Uuid>>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    // ========================================================================
    // Images - Jobs
    // ========================================================================

    /// List jobs for an image
    ///
    /// Returns workflow jobs associated with the specified image.
    #[endpoint {
        method = GET,
        path = "/images/{uuid}/jobs",
        tags = ["image-jobs"],
    }]
    async fn list_image_jobs(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<ListImageJobsQuery>,
    ) -> Result<HttpResponseOk<Vec<serde_json::Value>>, HttpError>;

    // ========================================================================
    // Images - Clone
    // ========================================================================

    /// Clone an image
    ///
    /// Creates a copy of the image for a different account. Only available
    /// in datacenter (dc) mode.
    #[endpoint {
        method = POST,
        path = "/images/{uuid}/clone",
        tags = ["images"],
    }]
    async fn clone_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<CloneImageQuery>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    // ========================================================================
    // Images - Admin Push
    // ========================================================================

    /// Push a Docker image (admin-only)
    ///
    /// Pushes a Docker image to a remote registry. Returns streaming JSON
    /// progress messages.
    #[endpoint {
        method = POST,
        path = "/images/{uuid}/push",
        tags = ["admin"],
    }]
    async fn admin_push_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<AdminPushQuery>,
    ) -> Result<Response<Body>, HttpError>;

    // ========================================================================
    // Auth Keys
    // ========================================================================

    /// Reload authentication keys (admin-only)
    ///
    /// Triggers a reload of authentication keys from the configured source.
    /// Returns an empty object `{}`.
    #[endpoint {
        method = POST,
        path = "/authkeys/reload",
        tags = ["admin"],
    }]
    async fn admin_reload_auth_keys(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    // ========================================================================
    // Datasets (Legacy Redirects)
    // ========================================================================

    /// List datasets (legacy redirect)
    ///
    /// Redirects to /images for backward compatibility with DSAPI.
    #[endpoint {
        method = GET,
        path = "/datasets",
        tags = ["legacy"],
    }]
    async fn list_datasets(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError>;

    /// Get a dataset (legacy redirect)
    ///
    /// Redirects to /images based on URN parsing. The `arg` parameter can
    /// be a UUID or a URN.
    #[endpoint {
        method = GET,
        path = "/datasets/{arg}",
        tags = ["legacy"],
    }]
    async fn get_dataset(
        rqctx: RequestContext<Self::Context>,
        path: Path<DatasetPath>,
    ) -> Result<Response<Body>, HttpError>;
}

/// Path parameter for legacy dataset endpoints
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DatasetPath {
    /// Dataset UUID or URN
    pub arg: String,
}
