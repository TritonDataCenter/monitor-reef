// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance delete command

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use dialoguer::Confirm;

#[derive(Args, Clone)]
pub struct DeleteArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,

    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,

    /// Wait for instance to be deleted
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

pub async fn run(args: DeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let id_str = machine_id.to_string();

        if !args.force
            && !Confirm::new()
                .with_prompt(format!("Delete instance {}?", &id_str[..8]))
                .default(false)
                .interact()?
        {
            println!("Skipping {}", &id_str[..8]);
            continue;
        }

        client
            .inner()
            .delete_machine()
            .account(account)
            .machine(machine_id)
            .send()
            .await?;

        println!("Deleting instance {}", &id_str[..8]);

        if args.wait {
            println!("Waiting for instance to be deleted...");
            super::wait::wait_for_state(machine_id, "deleted", args.wait_timeout, client).await?;
            println!("Instance {} deleted", &id_str[..8]);
        }
    }

    Ok(())
}
