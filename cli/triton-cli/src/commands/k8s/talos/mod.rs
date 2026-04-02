// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Talos client and operations
//!
//! This module provides a Rust client for Talos gRPC operations, including:
//! - mTLS connection setup with talosconfig credentials
//! - Bootstrap operations
//! - Health checks
//! - Kubeconfig retrieval
//! - Secret generation

pub mod bootstrap;
pub mod client;
pub mod config;
pub mod health;
pub mod kubeconfig;
pub mod proto;
pub mod retry;
pub mod talosconfig;
