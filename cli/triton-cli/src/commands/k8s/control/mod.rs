// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Control plane node management commands

use anyhow::Result;
use clap::Subcommand;
use cloudapi_client::TypedClient;

pub mod add;

#[derive(Subcommand, Clone)]
pub enum ControlCommand {
    /// Add control plane nodes to an existing cluster for HA
    Add(add::AddArgs),
}

impl ControlCommand {
    pub async fn run(self, client: &TypedClient, json: bool) -> Result<()> {
        match self {
            Self::Add(args) => add::run(args, client, json).await,
        }
    }
}
