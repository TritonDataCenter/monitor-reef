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
    pub instances: Vec<String>,
}

#[derive(Args, Clone)]
pub struct DisableProtectionArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,
}

pub async fn enable(args: EnableProtectionArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;

        client
            .enable_deletion_protection(account, &machine_id.parse()?, None)
            .await?;

        println!(
            "Enabled deletion protection for instance {}",
            &machine_id[..8]
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

        println!(
            "Disabled deletion protection for instance {}",
            &machine_id[..8]
        );
    }

    Ok(())
}
