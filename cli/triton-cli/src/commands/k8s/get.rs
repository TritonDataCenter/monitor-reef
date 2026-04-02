// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Get cluster details

use anyhow::Result;
use clap::Args;

use super::state::ClusterState;
use crate::output::json;

#[derive(Args, Clone)]
pub struct GetArgs {
    /// Cluster name or UUID
    pub cluster: String,
}

pub async fn run(args: GetArgs, use_json: bool) -> Result<()> {
    let cluster = ClusterState::load_by_name_or_uuid(&args.cluster).await?;

    if use_json {
        json::print_json(&cluster)?;
    } else {
        json::print_json_pretty(&cluster)?;
    }

    Ok(())
}
