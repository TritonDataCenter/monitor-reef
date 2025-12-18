// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance deletion protection commands

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

#[derive(Args, Clone)]
pub struct EnableProtectionArgs {
    /// Instance ID(s) or name(s)
    #[arg(required = true)]
    pub instances: Vec<String>,

    /// Wait for operation to complete (default: operation is synchronous)
    #[arg(short = 'w', long)]
    pub wait: bool,
}

#[derive(Args, Clone)]
pub struct DisableProtectionArgs {
    /// Instance ID(s) or name(s)
    #[arg(required = true)]
    pub instances: Vec<String>,

    /// Wait for operation to complete (default: operation is synchronous)
    #[arg(short = 'w', long)]
    pub wait: bool,
}

pub async fn enable(args: EnableProtectionArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;

        client
            .enable_deletion_protection(account, &machine_id.parse()?, None)
            .await?;

        // Use full UUID with quotes to match node-triton output format
        println!(
            "Enabled deletion protection for instance \"{}\"",
            machine_id
        );
    }

    Ok(())
}

pub async fn disable(args: DisableProtectionArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;

        client
            .disable_deletion_protection(account, &machine_id.parse()?, None)
            .await?;

        // Use full UUID with quotes to match node-triton output format
        println!(
            "Disabled deletion protection for instance \"{}\"",
            machine_id
        );
    }

    Ok(())
}
