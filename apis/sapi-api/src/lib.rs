// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SAPI (Services and Configuration API) trait definition
//!
//! This crate defines the API trait for Triton's SAPI service (version 2.1.3).
//! SAPI manages applications, services, instances, and configuration manifests
//! within a Triton datacenter.
//!
//! # API Hierarchy
//!
//! SAPI organizes its resources in a three-level hierarchy:
//! - **Applications** (top level, e.g., "sdc")
//! - **Services** (belong to an application, e.g., "manatee", "moray")
//! - **Instances** (belong to a service, each maps to a VM or agent zone)
//!
//! Configuration **manifests** define templates that are rendered with metadata
//! to produce configuration files for instances.
//!
//! # JSON Field Naming
//!
//! SAPI uses snake_case for all JSON field names, except PingResponse which
//! has camelCase `storType` and `storAvailable` fields.

use dropshot::{
    HttpError, HttpResponseDeleted, HttpResponseOk, HttpResponseUpdatedNoContent, Path, Query,
    RequestContext, TypedBody,
};

pub mod types;
pub use types::*;

/// SAPI trait definition
///
/// This trait defines all endpoints of the Triton SAPI service (version 2.1.3).
/// The API is organized into the following categories:
/// - Ping (health check)
/// - Mode (proto/full operating mode)
/// - Log Level (runtime log level management)
/// - Applications (top-level resource containers)
/// - Services (belong to applications)
/// - Instances (belong to services, map to VMs/agents)
/// - Manifests (configuration templates)
/// - Configs (assembled configuration for an instance)
/// - Cache (force sync of cached data)
#[dropshot::api_description]
pub trait SapiApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    // ========================================================================
    // Ping Endpoint
    // ========================================================================

    /// Health check
    ///
    /// Returns SAPI status including operating mode and storage availability.
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["health"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    // ========================================================================
    // Mode Endpoints
    // ========================================================================

    /// Get current operating mode
    ///
    /// Returns the current SAPI mode ("proto" or "full").
    #[endpoint {
        method = GET,
        path = "/mode",
        tags = ["mode"],
    }]
    async fn get_mode(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<SapiMode>, HttpError>;

    /// Set operating mode
    ///
    /// Transitions SAPI to the specified mode. Only "full" is accepted
    /// (transition from proto to full mode).
    #[endpoint {
        method = POST,
        path = "/mode",
        tags = ["mode"],
    }]
    async fn set_mode(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<SetModeBody>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // Log Level Endpoints
    // ========================================================================

    /// Get current log level
    #[endpoint {
        method = GET,
        path = "/loglevel",
        tags = ["loglevel"],
    }]
    async fn get_log_level(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<LogLevelResponse>, HttpError>;

    /// Set log level
    #[endpoint {
        method = POST,
        path = "/loglevel",
        tags = ["loglevel"],
    }]
    async fn set_log_level(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<SetLogLevelBody>,
    ) -> Result<HttpResponseOk<LogLevelResponse>, HttpError>;

    // ========================================================================
    // Application Endpoints
    // ========================================================================

    /// List applications
    ///
    /// Returns all applications, optionally filtered by name or owner.
    #[endpoint {
        method = GET,
        path = "/applications",
        tags = ["applications"],
    }]
    async fn list_applications(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListApplicationsQuery>,
    ) -> Result<HttpResponseOk<Vec<Application>>, HttpError>;

    /// Create an application
    ///
    /// Creates a new application. Requires `name` and `owner_uuid`.
    #[endpoint {
        method = POST,
        path = "/applications",
        tags = ["applications"],
    }]
    async fn create_application(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateApplicationBody>,
    ) -> Result<HttpResponseOk<Application>, HttpError>;

    /// Get an application
    ///
    /// Returns a single application by UUID.
    #[endpoint {
        method = GET,
        path = "/applications/{uuid}",
        tags = ["applications"],
    }]
    async fn get_application(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseOk<Application>, HttpError>;

    /// Update an application
    ///
    /// Updates application attributes. The `action` body field controls
    /// how attributes are modified (update/replace/delete). Default is "update".
    #[endpoint {
        method = PUT,
        path = "/applications/{uuid}",
        tags = ["applications"],
    }]
    async fn update_application(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
        body: TypedBody<UpdateApplicationBody>,
    ) -> Result<HttpResponseOk<Application>, HttpError>;

    /// Delete an application
    ///
    /// Deletes an application by UUID. Returns 204 on success.
    #[endpoint {
        method = DELETE,
        path = "/applications/{uuid}",
        tags = ["applications"],
    }]
    async fn delete_application(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Service Endpoints
    // ========================================================================

    /// List services
    ///
    /// Returns all services, optionally filtered by name, application, or type.
    #[endpoint {
        method = GET,
        path = "/services",
        tags = ["services"],
    }]
    async fn list_services(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListServicesQuery>,
    ) -> Result<HttpResponseOk<Vec<Service>>, HttpError>;

    /// Create a service
    ///
    /// Creates a new service. Requires `name` and `application_uuid`.
    #[endpoint {
        method = POST,
        path = "/services",
        tags = ["services"],
    }]
    async fn create_service(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateServiceBody>,
    ) -> Result<HttpResponseOk<Service>, HttpError>;

    /// Get a service
    ///
    /// Returns a single service by UUID.
    #[endpoint {
        method = GET,
        path = "/services/{uuid}",
        tags = ["services"],
    }]
    async fn get_service(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseOk<Service>, HttpError>;

    /// Update a service
    ///
    /// Updates service attributes. The `action` body field controls
    /// how attributes are modified (update/replace/delete). Default is "update".
    #[endpoint {
        method = PUT,
        path = "/services/{uuid}",
        tags = ["services"],
    }]
    async fn update_service(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
        body: TypedBody<UpdateServiceBody>,
    ) -> Result<HttpResponseOk<Service>, HttpError>;

    /// Delete a service
    ///
    /// Deletes a service by UUID. Returns 204 on success.
    #[endpoint {
        method = DELETE,
        path = "/services/{uuid}",
        tags = ["services"],
    }]
    async fn delete_service(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Instance Endpoints
    // ========================================================================

    /// List instances
    ///
    /// Returns all instances, optionally filtered by service or type.
    #[endpoint {
        method = GET,
        path = "/instances",
        tags = ["instances"],
    }]
    async fn list_instances(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListInstancesQuery>,
    ) -> Result<HttpResponseOk<Vec<Instance>>, HttpError>;

    /// Create an instance
    ///
    /// Creates a new instance. Requires `service_uuid`.
    /// If `async=true` query parameter is set, returns immediately with a
    /// `job_uuid` instead of waiting for provisioning to complete.
    #[endpoint {
        method = POST,
        path = "/instances",
        tags = ["instances"],
    }]
    async fn create_instance(
        rqctx: RequestContext<Self::Context>,
        query: Query<CreateInstanceQuery>,
        body: TypedBody<CreateInstanceBody>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Get an instance
    ///
    /// Returns a single instance by UUID.
    #[endpoint {
        method = GET,
        path = "/instances/{uuid}",
        tags = ["instances"],
    }]
    async fn get_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Get instance payload
    ///
    /// Returns the assembled zone parameters for provisioning. This is the
    /// merged result of application, service, and instance params/metadata.
    #[endpoint {
        method = GET,
        path = "/instances/{uuid}/payload",
        tags = ["instances"],
    }]
    async fn get_instance_payload(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Update an instance
    ///
    /// Updates instance attributes. The `action` body field controls
    /// how attributes are modified (update/replace/delete). Default is "update".
    #[endpoint {
        method = PUT,
        path = "/instances/{uuid}",
        tags = ["instances"],
    }]
    async fn update_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
        body: TypedBody<UpdateInstanceBody>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Upgrade an instance
    ///
    /// Upgrades an instance to a new image. Requires `image_uuid` in the body.
    #[endpoint {
        method = PUT,
        path = "/instances/{uuid}/upgrade",
        tags = ["instances"],
    }]
    async fn upgrade_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
        body: TypedBody<UpgradeInstanceBody>,
    ) -> Result<HttpResponseOk<Instance>, HttpError>;

    /// Delete an instance
    ///
    /// Deletes an instance by UUID. Returns 204 on success.
    #[endpoint {
        method = DELETE,
        path = "/instances/{uuid}",
        tags = ["instances"],
    }]
    async fn delete_instance(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Manifest Endpoints
    // ========================================================================

    /// List manifests
    ///
    /// Returns all configuration manifests.
    #[endpoint {
        method = GET,
        path = "/manifests",
        tags = ["manifests"],
    }]
    async fn list_manifests(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListManifestsQuery>,
    ) -> Result<HttpResponseOk<Vec<Manifest>>, HttpError>;

    /// Create a manifest
    ///
    /// Creates a new configuration manifest. Requires `name`, `path`, and `template`.
    #[endpoint {
        method = POST,
        path = "/manifests",
        tags = ["manifests"],
    }]
    async fn create_manifest(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateManifestBody>,
    ) -> Result<HttpResponseOk<Manifest>, HttpError>;

    /// Get a manifest
    ///
    /// Returns a single manifest by UUID.
    #[endpoint {
        method = GET,
        path = "/manifests/{uuid}",
        tags = ["manifests"],
    }]
    async fn get_manifest(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseOk<Manifest>, HttpError>;

    /// Delete a manifest
    ///
    /// Deletes a manifest by UUID. Returns 204 on success.
    #[endpoint {
        method = DELETE,
        path = "/manifests/{uuid}",
        tags = ["manifests"],
    }]
    async fn delete_manifest(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Config Endpoint
    // ========================================================================

    /// Get assembled config for an instance
    ///
    /// Returns the assembled configuration (manifests + metadata) for a
    /// specific instance. The response is freeform JSON since it depends
    /// on the instance's application/service/instance configuration hierarchy.
    ///
    /// The original Node.js endpoint also sets an ETag header for conditional
    /// request support.
    #[endpoint {
        method = GET,
        path = "/configs/{uuid}",
        tags = ["configs"],
    }]
    async fn get_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<UuidPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    // ========================================================================
    // Cache Endpoint
    // ========================================================================

    /// Sync cache
    ///
    /// Forces a synchronization of cached data. Returns 204 on success.
    #[endpoint {
        method = POST,
        path = "/cache",
        tags = ["cache"],
    }]
    async fn sync_cache(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;
}
