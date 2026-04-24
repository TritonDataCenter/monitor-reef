// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

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

    /// Wait for rename to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

pub async fn run(args: RenameArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
    let id_str = machine_id.to_string();

    client
        .rename_machine(
            account,
            &machine_id,
            &cloudapi_client::RenameMachineRequest {
                name: args.name.clone(),
                origin: None,
            },
        )
        .await?;

    println!("Renaming instance {} to {}", &id_str[..8], args.name);

    if args.wait {
        wait_for_rename(account, &machine_id, &args.name, args.wait_timeout, client).await?;
    }

    println!("Renamed instance {} to {}", &id_str[..8], args.name);

    Ok(())
}

async fn wait_for_rename(
    account: &str,
    machine_id: &uuid::Uuid,
    target_name: &str,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let machine = client.get_machine(account, machine_id).await?;

        if machine.name == target_name {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for rename to complete (current name: {})",
                machine.name,
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}
