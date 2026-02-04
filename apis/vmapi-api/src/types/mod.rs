// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! VMAPI type definitions
//!
//! Note: VMAPI uses snake_case for all JSON field names, not camelCase.
//! This is standard for Triton internal APIs.

pub mod common;
pub mod jobs;
pub mod metadata;
pub mod migrations;
pub mod role_tags;
pub mod statuses;
pub mod vms;

pub use common::*;
pub use jobs::*;
pub use metadata::*;
pub use migrations::*;
pub use role_tags::*;
pub use statuses::*;
pub use vms::*;
