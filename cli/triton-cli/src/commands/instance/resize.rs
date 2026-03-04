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

async fn wait_for_resize(
    account: &str,
    machine_id: &uuid::Uuid,
    target_package: &str,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let machine = client.get_machine(account, machine_id).await?;

        if machine.state == cloudapi_client::types::MachineState::Running
            && machine.package == target_package
        {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for resize to complete (current package: {})",
                machine.package,
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

pub async fn run(args: ResizeArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let package_id = crate::commands::package::resolve_package(&args.package, client).await?;
    let account = client.effective_account();
    let id_str = machine_id.to_string();

    client
        .resize_machine(account, &machine_id, package_id, None)
        .await?;

    println!(
        "Resizing instance {} to package {}",
        &id_str[..8],
        args.package
    );

    if args.wait {
        wait_for_resize(
            account,
            &machine_id,
            &args.package,
            args.wait_timeout,
            client,
        )
        .await?;
        println!("Resized instance {}", &id_str[..8]);
    }

    Ok(())
}
