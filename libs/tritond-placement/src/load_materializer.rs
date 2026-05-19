// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! ClickHouse → FDB `cn-load-summary` materialiser.
//!
//! Gated behind the `materializer` cargo feature so unit tests and
//! `make docker-up` builds don't have to pull in the CH client. PL-1
//! ships the module stub; PL-6 lands the leader-elected task body,
//! the per-CN ClickHouse SQL, and the metrics.
//!
//! See RFD 00005 doc 02 §"The load materialiser".
