// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

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
    let machine_uuid = resolve_instance(&args.instance, client).await?;

    let machine = client.get_machine(account, &machine_uuid).await?;

    if use_json {
        json::print_json(&machine)?;
    } else {
        json::print_json_pretty(&machine)?;
    }

    Ok(())
}

pub async fn ip(args: IpArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;
    let machine_uuid = resolve_instance(&args.instance, client).await?;

    let machine = client.get_machine(account, &machine_uuid).await?;

    if let Some(ip) = machine.primary_ip {
        println!("{}", ip);
    } else {
        return Err(anyhow::anyhow!("Instance has no primary IP"));
    }

    Ok(())
}

/// Resolve instance name or short ID to full UUID
pub async fn resolve_instance(id_or_name: &str, client: &TypedClient) -> Result<uuid::Uuid> {
    // First try as UUID
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        return Ok(uuid);
    }

    let account = &client.auth_config().account;

    // Try short ID match (at least 8 hex characters) — requires fetching all machines
    let is_short_uuid = id_or_name.len() >= 8
        && id_or_name
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-');
    if is_short_uuid {
        let response = client
            .inner()
            .list_machines()
            .account(account)
            .send()
            .await?;
        let machines = response.into_inner();
        let matches: Vec<_> = machines
            .iter()
            .filter(|m| m.id.to_string().starts_with(id_or_name))
            .collect();
        match matches.len() {
            1 => return Ok(matches[0].id),
            n if n > 1 => {
                let ids: Vec<String> = matches
                    .iter()
                    .map(|m| m.id.to_string()[..8].to_string())
                    .collect();
                return Err(anyhow::anyhow!(
                    "Ambiguous short ID '{}' matches {} instances: {}",
                    id_or_name,
                    n,
                    ids.join(", ")
                ));
            }
            _ => {} // No matches, fall through to name lookup
        }
    }

    // Try exact name match using server-side filter
    let response = client
        .inner()
        .list_machines()
        .account(account)
        .name(id_or_name)
        .send()
        .await?;
    let machines = response.into_inner();
    if let Some(m) = machines.first() {
        return Ok(m.id);
    }

    Err(anyhow::anyhow!("Instance not found: {}", id_or_name))
}
