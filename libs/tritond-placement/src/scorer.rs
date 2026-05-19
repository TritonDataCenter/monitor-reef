// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Built-in [`crate::Scorer`] implementations.
//!
//! PL-1 leaves this module empty. PL-4 lands the eight capacity
//! scorers + the four ClickHouse-load scorers (RFD 00005 doc 02
//! §"The built-in scorers") plus the `score-uniform-random`
//! deterministic-seed tie-break.
