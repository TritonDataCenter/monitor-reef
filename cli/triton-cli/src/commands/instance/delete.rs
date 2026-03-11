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
    let account = client.effective_account();
    let total = args.instances.len();
    let mut errors = Vec::new();

    for instance in &args.instances {
        let machine_id = match super::get::resolve_instance(instance, client).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Error: {}: {}", instance, e);
                errors.push(format!("{}: {}", instance, e));
                continue;
            }
        };
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

        if let Err(e) = client
            .inner()
            .delete_machine()
            .account(account)
            .machine(machine_id)
            .send()
            .await
        {
            #[cfg(debug_assertions)]
            if e.to_string()
                .contains(cloudapi_client::EMIT_PAYLOAD_SENTINEL)
            {
                continue;
            }
            eprintln!("Error deleting {}: {}", &id_str[..8], e);
            errors.push(format!("{}: {}", &id_str[..8], e));
            continue;
        }

        println!("Deleting instance {}", &id_str[..8]);

        if args.wait {
            println!("Waiting for instance to be deleted...");
            match super::wait::wait_for_state(
                machine_id,
                cloudapi_client::types::MachineState::Deleted,
                args.wait_timeout,
                client,
            )
            .await
            {
                Ok(()) => println!("Instance {} deleted", &id_str[..8]),
                Err(e) => {
                    eprintln!("Error waiting for {}: {}", &id_str[..8], e);
                    errors.push(format!("{}: {}", &id_str[..8], e));
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} of {} instances failed",
            errors.len(),
            total
        ))
    }
}
