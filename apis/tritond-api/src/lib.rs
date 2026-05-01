// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control-plane API.
//!
//! Phase 0 ships a deliberately small surface — a liveness check and
//! the silo CRUD primitives — that exercises the full Dropshot +
//! OpenAPI + Progenitor + FoundationDB pipeline end to end. Subsequent
//! phases extend the trait with `/v2/instances`, `/v2/audit`, and the
//! rest of DESIGN.md §14.
//!
//! Domain types live in [`tritond_store`] and are re-exported from
//! [`mod@types`] so wire types and storage types never drift.

pub mod types;

use dropshot::{HttpError, HttpResponseCreated, HttpResponseOk, Path, RequestContext, TypedBody};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::{NewSilo, Silo};

/// Liveness response.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Path parameters for endpoints that operate on a single silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloPath {
    pub silo_id: Uuid,
}

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

    /// Create a silo. Returns 201 with the created silo.
    ///
    /// Fails with 409 if a silo with the requested name already exists.
    #[endpoint {
        method = POST,
        path = "/v2/silos",
        tags = ["silos"],
    }]
    async fn create_silo(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSilo>,
    ) -> Result<HttpResponseCreated<Silo>, HttpError>;

    /// Look up a silo by id. Returns 404 if no such silo exists.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}",
        tags = ["silos"],
    }]
    async fn get_silo(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Silo>, HttpError>;
}
