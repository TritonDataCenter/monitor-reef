// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance resize command

use anyhow::Result;
use clap::Args;

use crate::client::AnyClient;
use crate::dispatch;

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

fn resize_body(package: &str) -> serde_json::Value {
    serde_json::json!({
        "action": "resize",
        "package": package,
    })
}

async fn wait_for_resize(
    account: &str,
    machine_id: &uuid::Uuid,
    target_package: &str,
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let (state_str, package): (String, String) = dispatch!(client, |c| {
            let resp = c
                .inner()
                .get_machine()
                .account(account)
                .machine(*machine_id)
                .send()
                .await?
                .into_inner();
            // `state` serializes as a lowercase wire string; compare as text.
            let state_str = serde_json::to_value(resp.state)?
                .as_str()
                .unwrap_or("")
                .to_string();
            (state_str, resp.package)
        });

        if state_str == "running" && package == target_package {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for resize to complete (current package: {})",
                package,
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

pub async fn run(args: ResizeArgs, client: &AnyClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let package_id = crate::commands::package::resolve_package(&args.package, client).await?;
    let account = client.effective_account();
    let id_str = machine_id.to_string();

    let body = resize_body(&package_id);
    dispatch!(client, |c| {
        c.inner()
            .update_machine()
            .account(account)
            .machine(machine_id)
            .body(body)
            .send()
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

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
