// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Job types for IMGAPI
//!
//! Jobs are returned from wfapi (workflow API). The structure is opaque
//! from IMGAPI's perspective -- it passes through whatever wfapi returns.

// Job objects from wfapi are opaque/variable. The ListImageJobs endpoint
// returns Vec<serde_json::Value>. The JobResponse type (with image_uuid
// and job_uuid) lives in common.rs.
