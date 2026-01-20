// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Agent communication for evacuate jobs
//!
//! This module handles HTTP communication with rebalancer agents running
//! on storage nodes.

// Agent communication is currently handled inline in the EvacuateJob methods.
// This module is a placeholder for future refactoring to separate concerns.
//
// Future enhancements could include:
// - Connection pooling per agent
// - Retry logic with exponential backoff
// - Circuit breaker pattern for unavailable agents
// - Agent health checking
