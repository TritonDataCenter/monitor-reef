// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance rename command

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

#[derive(Args, Clone)]
pub struct RenameArgs {
    /// Instance ID or name
    pub instance: String,

    /// New instance name (max 189 chars, or 63 if CNS enabled)
    pub name: String,
}

pub async fn run(args: RenameArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    client
        .rename_machine(account, &machine_id.parse()?, args.name.clone(), None)
        .await?;

    println!("Renamed instance {} to {}", &machine_id[..8], args.name);

    Ok(())
}
