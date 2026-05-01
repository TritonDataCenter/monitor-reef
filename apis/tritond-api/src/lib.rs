// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

use dropshot::{HttpError, HttpResponseOk, RequestContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Health check response.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Triton Cloud control-plane API.
///
/// Phase 0 surface is intentionally minimal: a single `/v2/health`
/// endpoint that demonstrates the Dropshot + OpenAPI + Progenitor flow
/// is wired correctly. Subsequent phases add `/v2/silos`,
/// `/v2/instances`, `/v2/audit`, and the rest of the surface from
/// DESIGN.md §14.
#[dropshot::api_description]
pub trait TritondApi {
    /// Context type for request handlers.
    type Context: Send + Sync + 'static;

    /// Liveness check. Returns service status and version string.
    #[endpoint {
        method = GET,
        path = "/v2/health",
        tags = ["system"],
    }]
    async fn health(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError>;
}
