// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::Result;
use clap::Subcommand;
use triton_gateway_client::TypedClient;

pub mod add;

#[derive(Subcommand, Clone)]
pub enum WorkerCommand {
    /// Provision and join new worker nodes to the cluster
    Add(add::WorkerAddArgs),
}

impl WorkerCommand {
    pub async fn run(self, client: &TypedClient, json: bool) -> Result<()> {
        match self {
            Self::Add(args) => add::run(args, client, json).await,
        }
    }
}
