// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SmartOS host adapters (`smartos::*`) and CN-status collection
//! (`cn_status::*`) shared across tritond compute-node tooling.
//!
//! Consumers (today: tritonagent) wire the [`cn_status::StatusSink`] trait
//! to whatever transport they use to publish back to the control plane.

pub mod cn_status;
pub mod smartos;
