// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::Result;
use clap::Subcommand;
use triton_gateway_client::TypedClient;

pub mod install;
pub mod remove;
pub mod status;

#[derive(Subcommand, Clone)]
pub enum LbCommand {
    /// Install the Triton LB controller into a cluster
    Install(install::InstallArgs),
    /// Show LB controller status for a cluster
    Status(status::StatusArgs),
    /// Remove the LB controller from a cluster
    Remove(remove::RemoveArgs),
}

impl LbCommand {
    pub async fn run(self, client: &TypedClient, json: bool) -> Result<()> {
        match self {
            Self::Install(args) => install::run(args, client, json).await,
            Self::Status(args) => status::run(args, client, json).await,
            Self::Remove(args) => remove::run(args, client, json).await,
        }
    }
}
