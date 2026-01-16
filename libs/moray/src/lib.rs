// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Moray client library for interacting with Moray key-value stores.
//!
//! This crate provides an async client for the Moray service, which is a
//! JSON-based key-value store built on top of PostgreSQL. The client uses
//! the Fast RPC protocol for communication.

pub mod buckets;
pub mod client;
pub mod connector;
pub mod meta;
pub mod objects;
