// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance lifecycle commands (start, stop, reboot)

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

#[derive(Args, Clone)]
pub struct StartArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,

    /// Wait for instance to be running
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct StopArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,

    /// Wait for instance to be stopped
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct RebootArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,

    /// Wait for instance to be running
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

pub async fn start(args: StartArgs, client: &TypedClient) -> Result<()> {
    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let account = &client.auth_config().account;

        client
            .start_machine(account, &machine_id.parse()?, None)
            .await?;

        println!("Starting instance {}", &machine_id[..8]);

        if args.wait {
            super::wait::wait_for_state(&machine_id, "running", args.wait_timeout, client).await?;
            println!("Instance {} is running", &machine_id[..8]);
        }
    }
    Ok(())
}

pub async fn stop(args: StopArgs, client: &TypedClient) -> Result<()> {
    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let account = &client.auth_config().account;

        client
            .stop_machine(account, &machine_id.parse()?, None)
            .await?;

        println!("Stopping instance {}", &machine_id[..8]);

        if args.wait {
            super::wait::wait_for_state(&machine_id, "stopped", args.wait_timeout, client).await?;
            println!("Instance {} is stopped", &machine_id[..8]);
        }
    }
    Ok(())
}

pub async fn reboot(args: RebootArgs, client: &TypedClient) -> Result<()> {
    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let account = &client.auth_config().account;

        client
            .reboot_machine(account, &machine_id.parse()?, None)
            .await?;

        println!("Rebooting instance {}", &machine_id[..8]);

        if args.wait {
            super::wait::wait_for_state(&machine_id, "running", args.wait_timeout, client).await?;
            println!("Instance {} is running", &machine_id[..8]);
        }
    }
    Ok(())
}
