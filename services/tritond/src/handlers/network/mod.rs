// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! VPC-scoped network handlers.

pub(crate) mod dhcp;
pub(crate) mod firewall;
pub(crate) mod nat;
pub(crate) mod operator;
pub(crate) mod routes;
pub(crate) mod subnet;
pub(crate) mod vpc;
