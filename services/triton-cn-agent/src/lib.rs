// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Compute Node Agent service (Rust port of `sdc-cn-agent`).
//!
//! The service wires together:
//! * A task registry keyed by [`cn_agent_api::TaskName`].
//! * An in-memory history ring buffer (16 entries, matching the Node.js agent).
//! * A pause flag that makes `/tasks` return 503 while set.
//!
//! Platform-specific tasks (SmartOS `vmadm`/`zfs` wrappers, etc.) live in the
//! `tasks` module; for portability, only platform-independent tasks are
//! compiled on non-illumos builds so the crate can be built and tested on
//! developer laptops.

pub mod api_impl;
pub mod context;
pub mod registry;
pub mod tasks;

pub use context::{AgentContext, AgentMetadata};
pub use registry::{TaskHandler, TaskRegistry};

/// Maximum number of entries retained in the task history buffer.
///
/// Matches the legacy Node.js agent's `maxHistory = 16`, so operators running
/// `curl /history` see the same window on either implementation.
pub const TASK_HISTORY_SIZE: usize = 16;

/// Default TCP port the agent binds when nothing is configured.
///
/// Legacy agent listens on 5309 by default; CNAPI discovers non-default ports
/// via the `CN Agent Port` field in sysinfo.
pub const DEFAULT_AGENT_PORT: u16 = 5309;
