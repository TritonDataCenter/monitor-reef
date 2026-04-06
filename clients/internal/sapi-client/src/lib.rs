// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SAPI Client Library
//!
//! This client provides typed access to the Triton SAPI service
//! (Services and Configuration API).
//!
//! ## Usage
//!
//! ```ignore
//! use sapi_client::Client;
//!
//! let client = Client::new("http://sapi.my-dc.my-cloud.local");
//!
//! // List applications
//! let apps = client.list_applications().send().await?;
//!
//! // Get a specific service
//! let svc = client.get_service().uuid(service_uuid).send().await?;
//!
//! // Update an instance with merge semantics (default action)
//! use sapi_client::UpdateInstanceBody;
//! let body = UpdateInstanceBody {
//!     action: None, // defaults to "update"
//!     params: Some(new_params),
//!     metadata: None,
//!     manifests: None,
//! };
//! client.update_instance().uuid(instance_uuid).body(body).send().await?;
//! ```

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

// Re-export types from the API crate for convenience.
pub use sapi_api::{
    // Application types
    Application,
    CreateApplicationBody,
    // Instance types
    CreateInstanceBody,
    CreateInstanceQuery,
    // Manifest types
    CreateManifestBody,
    // Service types
    CreateServiceBody,
    Instance,
    ListApplicationsQuery,
    ListInstancesQuery,
    ListManifestsQuery,
    ListServicesQuery,
    // Ops types
    LogLevelResponse,
    Manifest,
    ModeResponse,
    PingResponse,
    SapiMode,
    Service,
    // Common types
    ServiceType,
    SetLogLevelBody,
    SetModeBody,
    UpdateAction,
    UpdateApplicationBody,
    UpdateAttributesBody,
    UpdateInstanceBody,
    UpdateServiceBody,
    UpgradeInstanceBody,
    Uuid,
    UuidPath,
};
