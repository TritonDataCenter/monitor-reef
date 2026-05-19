// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Built-in [`crate::Filter`] implementations.
//!
//! PL-1 leaves this module empty. PL-3 lands the seventeen built-in
//! filters (RFD 00005 doc 02 §"The seventeen built-in filters") as
//! one `impl Filter` per module item plus a per-filter unit test.
//! The opt-in `cn-load-not-overheating` guardrail filter also lives
//! here.
