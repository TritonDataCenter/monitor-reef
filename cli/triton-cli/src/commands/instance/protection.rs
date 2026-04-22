// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance deletion protection commands

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args;
use tokio::time::sleep;

use crate::client::AnyClient;
use crate::dispatch;

#[derive(Args, Clone)]
pub struct EnableProtectionArgs {
    /// Instance ID(s) or name(s)
    #[arg(required = true)]
    pub instances: Vec<String>,

    /// Wait for operation to complete (default: operation is synchronous)
    #[arg(short = 'w', long)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "120")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct DisableProtectionArgs {
    /// Instance ID(s) or name(s)
    #[arg(required = true)]
    pub instances: Vec<String>,

    /// Wait for operation to complete (default: operation is synchronous)
    #[arg(short = 'w', long)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "120")]
    pub wait_timeout: u64,
}

fn protection_body(enabled: bool) -> serde_json::Value {
    serde_json::json!({
        "action": if enabled {
            "enable_deletion_protection"
        } else {
            "disable_deletion_protection"
        },
    })
}

pub async fn enable(args: EnableProtectionArgs, client: &AnyClient) -> Result<()> {
    let account = client.effective_account();

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;

        let body = protection_body(true);
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

        if args.wait {
            wait_for_protection(account, &machine_id, true, args.wait_timeout, client).await?;
        }

        // Use full UUID with quotes to match node-triton output format
        println!(
            "Enabled deletion protection for instance \"{}\"",
            machine_id
        );
    }

    Ok(())
}

pub async fn disable(args: DisableProtectionArgs, client: &AnyClient) -> Result<()> {
    let account = client.effective_account();

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;

        let body = protection_body(false);
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

        if args.wait {
            wait_for_protection(account, &machine_id, false, args.wait_timeout, client).await?;
        }

        // Use full UUID with quotes to match node-triton output format
        println!(
            "Disabled deletion protection for instance \"{}\"",
            machine_id
        );
    }

    Ok(())
}

async fn wait_for_protection(
    account: &str,
    machine_id: &uuid::Uuid,
    expect_enabled: bool,
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<()> {
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let is_enabled: bool = dispatch!(client, |c| {
            let resp = c
                .inner()
                .get_machine()
                .account(account)
                .machine(*machine_id)
                .send()
                .await?
                .into_inner();
            resp.deletion_protection.unwrap_or(false)
        });

        if is_enabled == expect_enabled {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for deletion protection to be {}",
                if expect_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}
