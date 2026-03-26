// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use dropshot::{HttpError, HttpResponseOk, RequestContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Ping response
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {}

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
