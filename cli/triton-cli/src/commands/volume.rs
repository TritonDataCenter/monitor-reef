// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Volume management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum VolumeCommand {
    /// List volumes
    #[command(alias = "ls")]
    List,
    /// Get volume details
    Get(VolumeGetArgs),
    /// Create volume
    Create(VolumeCreateArgs),
    /// Delete volume(s)
    #[command(alias = "rm")]
    Delete(VolumeDeleteArgs),
    /// List available volume sizes
    Sizes,
}

#[derive(Args, Clone)]
pub struct VolumeGetArgs {
    /// Volume ID or name
    pub volume: String,
}

#[derive(Args, Clone)]
pub struct VolumeCreateArgs {
    /// Volume name
    #[arg(long)]
    pub name: String,
    /// Volume size in MB (e.g., 10240 for 10GB)
    #[arg(long)]
    pub size: i64,
    /// Volume type
    #[arg(long, default_value = "tritonnfs")]
    pub r#type: String,
    /// Network ID(s)
    #[arg(long)]
    pub network: Option<Vec<String>>,
}

#[derive(Args, Clone)]
pub struct VolumeDeleteArgs {
    /// Volume ID(s) or name(s)
    pub volumes: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
    /// Wait for deletion
    #[arg(long, short)]
    pub wait: bool,
}

impl VolumeCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_volumes(client, use_json).await,
            Self::Get(args) => get_volume(args, client, use_json).await,
            Self::Create(args) => create_volume(args, client, use_json).await,
            Self::Delete(args) => delete_volumes(args, client).await,
            Self::Sizes => list_volume_sizes(client, use_json).await,
        }
    }
}

async fn list_volumes(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_volumes()
        .account(account)
        .send()
        .await?;

    let volumes = response.into_inner();

    if use_json {
        json::print_json(&volumes)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "SIZE", "STATE", "TYPE"]);
        for vol in &volumes {
            tbl.add_row(vec![
                &vol.id.to_string()[..8],
                &vol.name,
                &format!("{} MB", vol.size),
                &format!("{:?}", vol.state).to_lowercase(),
                &vol.type_,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_volume(args: VolumeGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let volume_id = resolve_volume(&args.volume, client).await?;

    let response = client
        .inner()
        .get_volume()
        .account(account)
        .id(&volume_id)
        .send()
        .await?;

    let volume = response.into_inner();

    if use_json {
        json::print_json(&volume)?;
    } else {
        println!("ID:      {}", volume.id);
        println!("Name:    {}", volume.name);
        println!("Size:    {} MB", volume.size);
        println!("State:   {:?}", volume.state);
        println!("Type:    {}", volume.type_);
        if !volume.networks.is_empty() {
            println!("Networks: {:?}", volume.networks);
        }
        println!("Created: {}", volume.created);
    }

    Ok(())
}

async fn create_volume(args: VolumeCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let networks = args.network.clone();

    let request = cloudapi_client::types::CreateVolumeRequest {
        name: Some(args.name.clone()),
        type_: Some(args.r#type.clone()),
        size: args.size as u64,
        networks,
        tags: None,
    };

    let response = client
        .inner()
        .create_volume()
        .account(account)
        .body(request)
        .send()
        .await?;
    let volume = response.into_inner();

    println!(
        "Created volume {} ({}) - {} MB",
        volume.name,
        &volume.id.to_string()[..8],
        volume.size
    );

    if use_json {
        json::print_json(&volume)?;
    }

    Ok(())
}

async fn delete_volumes(args: VolumeDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for volume_name in &args.volumes {
        let volume_id = resolve_volume(volume_name, client).await?;

        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete volume '{}'?", volume_name))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        client
            .inner()
            .delete_volume()
            .account(account)
            .id(&volume_id)
            .send()
            .await?;

        println!("Deleting volume {}", volume_name);

        if args.wait {
            wait_for_volume_deletion(&volume_id, client).await?;
            println!("Volume {} deleted", volume_name);
        }
    }

    Ok(())
}

async fn list_volume_sizes(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_volume_sizes()
        .account(account)
        .send()
        .await?;

    let sizes = response.into_inner();

    if use_json {
        json::print_json(&sizes)?;
    } else {
        let mut tbl = table::create_table(&["SIZE"]);
        for size in &sizes {
            tbl.add_row(vec![&format!("{} GB", size.size)]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

/// Resolve volume name or short ID to full UUID
pub async fn resolve_volume(id_or_name: &str, client: &TypedClient) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_volumes()
        .account(account)
        .send()
        .await?;

    let volumes = response.into_inner();

    // Try short ID match first (at least 8 characters)
    if id_or_name.len() >= 8 {
        for vol in &volumes {
            if vol.id.to_string().starts_with(id_or_name) {
                return Ok(vol.id.to_string());
            }
        }
    }

    // Try exact name match
    for vol in &volumes {
        if vol.name == id_or_name {
            return Ok(vol.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Volume not found: {}", id_or_name))
}

async fn wait_for_volume_deletion(volume_id: &str, client: &TypedClient) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = &client.auth_config().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(300);

    loop {
        let result = client
            .inner()
            .get_volume()
            .account(account)
            .id(volume_id)
            .send()
            .await;

        match result {
            Ok(response) => {
                let volume = response.into_inner();
                let state = format!("{:?}", volume.state).to_lowercase();
                if state == "failed" {
                    return Err(anyhow::anyhow!("Volume deletion failed"));
                }
            }
            Err(_) => {
                // Volume not found means it's deleted
                return Ok(());
            }
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for volume deletion"));
        }

        sleep(Duration::from_secs(2)).await;
    }
}
