// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance get and IP commands

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

use crate::output::json;

#[derive(Args, Clone)]
pub struct GetArgs {
    /// Instance ID or name
    pub instance: String,
}

#[derive(Args, Clone)]
pub struct IpArgs {
    /// Instance ID or name
    pub instance: String,
}

pub async fn run(args: GetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let machine_id = resolve_instance(&args.instance, client).await?;

    let response = client
        .inner()
        .get_machine()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let machine = response.into_inner();

    if use_json {
        json::print_json(&machine)?;
    } else {
        println!("ID:          {}", machine.id);
        println!("Name:        {}", machine.name);
        println!("State:       {:?}", machine.state);
        println!("Image:       {}", machine.image);
        println!("Package:     {}", machine.package);
        println!("Memory:      {} MB", machine.memory);
        println!(
            "Primary IP:  {}",
            machine.primary_ip.as_deref().unwrap_or("-")
        );
        println!("Created:     {}", machine.created);
        if machine.firewall_enabled.unwrap_or(false) {
            println!("Firewall:    enabled");
        }
        if machine.deletion_protection.unwrap_or(false) {
            println!("Deletion Protection: enabled");
        }
    }

    Ok(())
}

pub async fn ip(args: IpArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;
    let machine_id = resolve_instance(&args.instance, client).await?;

    let response = client
        .inner()
        .get_machine()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let machine = response.into_inner();

    if let Some(ip) = machine.primary_ip {
        println!("{}", ip);
    } else {
        return Err(anyhow::anyhow!("Instance has no primary IP"));
    }

    Ok(())
}

/// Resolve instance name or short ID to full UUID
pub async fn resolve_instance(id_or_name: &str, client: &TypedClient) -> Result<String> {
    // First try as UUID
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    // Try as short ID or name
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_machines()
        .account(account)
        .send()
        .await?;

    let machines = response.into_inner();

    // Try short ID match (at least 8 characters)
    if id_or_name.len() >= 8 {
        for m in &machines {
            if m.id.to_string().starts_with(id_or_name) {
                return Ok(m.id.to_string());
            }
        }
    }

    // Try exact name match
    for m in &machines {
        if m.name == id_or_name {
            return Ok(m.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Instance not found: {}", id_or_name))
}
