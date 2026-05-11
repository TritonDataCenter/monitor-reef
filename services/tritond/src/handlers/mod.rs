// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! HTTP handler modules; the `TritondApi` impl delegates to these.

pub(crate) mod agents;
pub(crate) mod auth_keys;
pub(crate) mod cns;
pub(crate) mod config;
pub(crate) mod health;
pub(crate) mod images;
pub(crate) mod instances;
pub(crate) mod legacy;
pub(crate) mod network;
pub(crate) mod projects;
pub(crate) mod silos;
pub(crate) mod ssh_keys;
pub(crate) mod storage_clusters;
pub(crate) mod telemetry;
pub(crate) mod tenants;
