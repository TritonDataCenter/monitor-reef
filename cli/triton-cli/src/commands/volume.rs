// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Volume management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct VolumeListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Args, Clone)]
pub struct VolumeSizesArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Subcommand, Clone)]
pub enum VolumeCommand {
    /// List volumes
    #[command(alias = "ls")]
    List(VolumeListArgs),
    /// Get volume details
    Get(VolumeGetArgs),
    /// Create volume
    Create(VolumeCreateArgs),
    /// Delete volume(s)
    #[command(alias = "rm")]
    Delete(VolumeDeleteArgs),
    /// List available volume sizes
    Sizes(VolumeSizesArgs),
}

#[derive(Args, Clone)]
pub struct VolumeGetArgs {
    /// Volume ID or name
    pub volume: String,
}

#[derive(Args, Clone)]
pub struct VolumeCreateArgs {
    /// Volume name (optional, generated server-side if not provided)
    #[arg(long, short = 'n')]
    pub name: Option<String>,

    /// Volume size in gibibytes (e.g., "20G") or megabytes (e.g., 10240)
    #[arg(long, short = 's')]
    pub size: Option<String>,

    /// Volume type (default: tritonnfs)
    #[arg(long, short = 't', default_value = "tritonnfs")]
    pub r#type: String,

    /// Network ID, name, or short ID (uses default fabric network if not specified)
    #[arg(long, short = 'N')]
    pub network: Option<String>,

    /// Tags in key=value format (can be specified multiple times)
    #[arg(long = "tag")]
    pub tags: Option<Vec<String>>,

    /// Affinity rules for server selection (can be specified multiple times)
    #[arg(long, short = 'a')]
    pub affinity: Option<Vec<String>>,

    /// Wait for creation to complete (use multiple times for spinner)
    #[arg(long, short = 'w', action = clap::ArgAction::Count)]
    pub wait: u8,

    /// Timeout in seconds when waiting
    #[arg(long = "wait-timeout")]
    pub wait_timeout: Option<u64>,
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
    /// Wait timeout in seconds
    #[arg(long, default_value = "300")]
    pub wait_timeout: u64,
}

impl VolumeCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_volumes(args, client, use_json).await,
            Self::Get(args) => get_volume(args, client, use_json).await,
            Self::Create(args) => create_volume(args, client, use_json).await,
            Self::Delete(args) => delete_volumes(args, client).await,
            Self::Sizes(args) => list_volume_sizes(args, client, use_json).await,
        }
    }
}

async fn list_volumes(args: VolumeListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
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
        let mut tbl = TableBuilder::new(&["SHORTID", "NAME", "SIZE", "STATE", "TYPE"])
            .with_long_headers(&["ID", "CREATED"]);
        for vol in &volumes {
            tbl.add_row(vec![
                vol.id.to_string()[..8].to_string(),
                vol.name.clone(),
                format!("{} MB", vol.size),
                format!("{:?}", vol.state).to_lowercase(),
                vol.type_.clone(),
                vol.id.to_string(),
                vol.created.to_string(),
            ]);
        }
        tbl.print(&args.table);
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

/// Parse volume size from string, supporting GiB format ("20G") or plain MB
fn parse_volume_size(size_str: &str) -> Result<u64> {
    // Check for GiB format (e.g., "20G")
    if let Some(gib_str) = size_str.strip_suffix('G') {
        let gib: u64 = gib_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid size format: {}", size_str))?;
        if gib == 0 {
            return Err(anyhow::anyhow!("Size must be greater than 0"));
        }
        // 1 GiB = 1024 MiB
        Ok(gib * 1024)
    } else {
        // Plain MB format
        size_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid size format: {}", size_str))
    }
}

/// Parse tags from key=value format into a serde_json Map
fn parse_tags(tag_list: &[String]) -> serde_json::Map<String, serde_json::Value> {
    let mut tags = serde_json::Map::new();
    for tag in tag_list {
        if let Some((key, value)) = tag.split_once('=') {
            // Try to parse as bool or number, otherwise use string
            let json_value = if value == "true" {
                serde_json::Value::Bool(true)
            } else if value == "false" {
                serde_json::Value::Bool(false)
            } else if let Ok(num) = value.parse::<i64>() {
                serde_json::Value::Number(num.into())
            } else if let Ok(num) = value.parse::<f64>() {
                serde_json::json!(num)
            } else {
                serde_json::Value::String(value.to_string())
            };
            tags.insert(key.to_string(), json_value);
        }
    }
    tags
}

async fn create_volume(args: VolumeCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // Warn about affinity if specified (not currently supported by API)
    if args.affinity.is_some() {
        eprintln!("Warning: --affinity option is not currently supported by the API");
    }

    // Parse size
    let size = if let Some(size_str) = &args.size {
        parse_volume_size(size_str)?
    } else {
        // Use smallest available size (default behavior per node-triton)
        let sizes_response = client
            .inner()
            .list_volume_sizes()
            .account(account)
            .send()
            .await?;
        let sizes = sizes_response.into_inner();
        sizes
            .iter()
            .map(|s| s.size * 1024) // Convert GB to MB
            .min()
            .unwrap_or(10 * 1024) // Fallback to 10 GB
    };

    // Handle network - resolve name/shortid to UUID if provided
    let networks = if let Some(net) = &args.network {
        let network_id = crate::commands::network::resolve_network(net, client).await?;
        Some(vec![network_id])
    } else {
        None
    };

    // Parse tags
    let tags = args.tags.as_ref().map(|t| parse_tags(t));

    let request = cloudapi_client::types::CreateVolumeRequest {
        name: args.name.clone(),
        type_: Some(args.r#type.clone()),
        size,
        networks,
        tags,
    };

    let response = client
        .inner()
        .create_volume()
        .account(account)
        .body(request)
        .send()
        .await?;
    let volume = response.into_inner();

    let should_wait = args.wait > 0;
    let wait_timeout = args.wait_timeout.unwrap_or(300); // Default 5 minutes

    if should_wait {
        println!(
            "Creating volume {} ({})...",
            volume.name,
            &volume.id.to_string()[..8]
        );

        let final_volume =
            wait_for_volume_ready(&volume.id.to_string(), client, wait_timeout).await?;

        if use_json {
            json::print_json(&final_volume)?;
        } else {
            let state = format!("{:?}", final_volume.state).to_lowercase();
            if state == "ready" {
                println!(
                    "Created volume {} ({}) - {} MB",
                    final_volume.name,
                    &final_volume.id.to_string()[..8],
                    final_volume.size
                );
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to create volume {} ({})",
                    final_volume.name,
                    final_volume.id
                ));
            }
        }
    } else {
        println!(
            "Creating volume {} ({}) - {} MB",
            volume.name,
            &volume.id.to_string()[..8],
            volume.size
        );

        if use_json {
            json::print_json(&volume)?;
        }
    }

    Ok(())
}

async fn wait_for_volume_ready(
    volume_id: &str,
    client: &TypedClient,
    timeout_secs: u64,
) -> Result<cloudapi_client::types::Volume> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = &client.auth_config().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let response = client
            .inner()
            .get_volume()
            .account(account)
            .id(volume_id)
            .send()
            .await?;

        let volume = response.into_inner();
        let state = format!("{:?}", volume.state).to_lowercase();

        if state == "ready" || state == "failed" {
            return Ok(volume);
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for volume to become ready"
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
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
            wait_for_volume_deletion(&volume_id, client, args.wait_timeout).await?;
            println!("Volume {} deleted", volume_name);
        }
    }

    Ok(())
}

async fn list_volume_sizes(
    args: VolumeSizesArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
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
        let mut tbl = TableBuilder::new(&["SIZE"]);
        for size in &sizes {
            tbl.add_row(vec![format!("{} GB", size.size)]);
        }
        tbl.print(&args.table);
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

async fn wait_for_volume_deletion(
    volume_id: &str,
    client: &TypedClient,
    timeout_secs: u64,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = &client.auth_config().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

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
