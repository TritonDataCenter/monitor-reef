// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! cn-agent client library.
//!
//! Typed HTTP client for Triton's Compute Node Agent, used by CNAPI and other
//! control-plane services to dispatch tasks against a specific CN.
//!
//! # Example
//!
//! ```ignore
//! use cn_agent_client::{Client, TaskName};
//!
//! let client = Client::new("http://<admin-ip>:5309");
//!
//! let resp = client
//!     .dispatch_task()
//!     .body(serde_json::json!({
//!         "task": "nop",
//!         "params": {}
//!     }))
//!     .send()
//!     .await?;
//! ```

// Progenitor's generated code uses unwrap() in a couple of spots (notably in
// Client::new). We only silence it for that module.
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

// Re-export canonical API types so callers only need to depend on this crate.
pub use cn_agent_api::{
    MachineUuidParams, PingResponse, SleepParams, TaskError, TaskHistoryEntry, TaskHistoryResponse,
    TaskName, TaskRequest, TaskStatus, Uuid,
};
