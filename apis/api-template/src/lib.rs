// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

use dropshot::{HttpError, HttpResponseOk, RequestContext, endpoint};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Health check response
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Example API trait
///
/// Define your API endpoints here. Each endpoint should be a method on the trait
/// with the appropriate #[endpoint] attribute.
#[dropshot::api_description]
pub trait ExampleApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    /// Health check endpoint
    #[endpoint {
        method = GET,
        path = "/health",
        tags = ["system"],
    }]
    async fn health(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError>;

    // Add more endpoints here following the same pattern
}
