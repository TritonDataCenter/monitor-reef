// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance resize command

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

#[derive(Args, Clone)]
pub struct ResizeArgs {
    /// Instance ID or name
    pub instance: String,

    /// New package name or UUID
    pub package: String,

    /// Wait for resize to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

pub async fn run(args: ResizeArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;
    let id_str = machine_id.to_string();

    client
        .resize_machine(account, &machine_id, args.package.clone(), None)
        .await?;

    println!(
        "Resizing instance {} to package {}",
        &id_str[..8],
        args.package
    );

    if args.wait {
        println!("Waiting for resize to complete...");
        super::wait::wait_for_state(machine_id, "running", args.wait_timeout, client).await?;
        println!("Instance {} resize complete", &id_str[..8]);
    }

    Ok(())
}
