// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton API trait definition
//!
//! This crate defines the API trait for the Triton API service.
//! It serves as the public-facing HTTP API for the Triton datacenter.

use dropshot::{HttpError, HttpResponseOk, RequestContext};

pub mod types;
pub use types::*;

/// Triton API
#[dropshot::api_description]
pub trait TritonApi {
    type Context: Send + Sync + 'static;

    /// Ping
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["system"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;
}
