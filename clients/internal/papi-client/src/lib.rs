// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! PAPI Client Library
//!
//! This client provides typed access to the Triton PAPI service
//! (Packages API).
//!
//! ## Usage
//!
//! ```ignore
//! use papi_client::Client;
//!
//! let client = Client::new("http://papi.my-dc.my-cloud.local");
//!
//! // List packages
//! let packages = client.list_packages().send().await?;
//!
//! // Get a specific package
//! let pkg = client.get_package().uuid(package_uuid).send().await?;
//!
//! // Create a package
//! use papi_client::CreatePackageRequest;
//! let req = CreatePackageRequest { .. };
//! client.create_package().body(req).send().await?;
//! ```

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

// Re-export types from the API crate for convenience.
pub use papi_api::{
    // Enums
    AllocServerSpread,
    BackendStatus,
    Brand,
    // Structs
    CreatePackageRequest,
    DeletePackageQuery,
    DiskSize,
    DiskSizeRemaining,
    DiskSpec,
    GetPackageQuery,
    ListPackagesQuery,
    Package,
    PackagePath,
    PingResponse,
    SortOrder,
    UpdatePackageRequest,
    // Common types
    Uuid,
};
