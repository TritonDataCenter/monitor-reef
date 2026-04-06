// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Packages API (PAPI) trait definition.
//!
//! PAPI manages package definitions (RAM, CPU, disk, etc.) used by other
//! services to provision VMs.

pub mod types;

pub use types::*;

use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseOk, HttpResponseUpdatedNoContent, Path, Query,
    RequestContext, TypedBody,
};

/// Triton Packages API
///
/// Manages package definitions that specify resource allocations for
/// virtual machine provisioning.
#[dropshot::api_description]
pub trait PapiApi {
    type Context: Send + Sync + 'static;

    // ========================================================================
    // Health
    // ========================================================================

    /// Ping the PAPI service
    ///
    /// Returns the service process ID and backend (Moray) health status.
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["health"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    // ========================================================================
    // Packages
    // ========================================================================

    /// List packages
    ///
    /// Returns an array of packages matching the query filters. Supports
    /// filtering by any package field, sorting, and pagination.
    #[endpoint {
        method = GET,
        path = "/packages",
        tags = ["packages"],
    }]
    async fn list_packages(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListPackagesQuery>,
    ) -> Result<HttpResponseOk<Vec<Package>>, HttpError>;

    /// Get a package by UUID
    ///
    /// Returns a single package. The optional `owner_uuids` query parameter
    /// can be used to filter by ownership.
    #[endpoint {
        method = GET,
        path = "/packages/{uuid}",
        tags = ["packages"],
    }]
    async fn get_package(
        rqctx: RequestContext<Self::Context>,
        path: Path<PackagePath>,
        query: Query<GetPackageQuery>,
    ) -> Result<HttpResponseOk<Package>, HttpError>;

    /// Create a new package
    ///
    /// Creates a package with the given resource definitions. Returns the
    /// created package with server-assigned fields (uuid if not provided,
    /// timestamps, version number).
    #[endpoint {
        method = POST,
        path = "/packages",
        tags = ["packages"],
    }]
    async fn create_package(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreatePackageRequest>,
    ) -> Result<HttpResponseCreated<Package>, HttpError>;

    /// Update a package
    ///
    /// Updates mutable fields on a package. Immutable fields can only be
    /// changed when `force` is set to true in the request body.
    #[endpoint {
        method = PUT,
        path = "/packages/{uuid}",
        tags = ["packages"],
    }]
    async fn update_package(
        rqctx: RequestContext<Self::Context>,
        path: Path<PackagePath>,
        body: TypedBody<UpdatePackageRequest>,
    ) -> Result<HttpResponseOk<Package>, HttpError>;

    /// Delete a package
    ///
    /// Deletes a package. Requires `force=true` as a query parameter;
    /// otherwise returns 405.
    #[endpoint {
        method = DELETE,
        path = "/packages/{uuid}",
        tags = ["packages"],
    }]
    async fn delete_package(
        rqctx: RequestContext<Self::Context>,
        path: Path<PackagePath>,
        query: Query<DeletePackageQuery>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;
}
