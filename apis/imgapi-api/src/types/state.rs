// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Admin state types for IMGAPI

// Admin state snapshot is an opaque internal structure. We use
// serde_json::Value in the API trait return type directly, so no
// additional types are needed here. The StateAction and StateActionQuery
// types live in action.rs.
