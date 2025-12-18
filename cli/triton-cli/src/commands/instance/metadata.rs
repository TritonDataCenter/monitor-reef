// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance metadata subcommands

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use cloudapi_client::types::AddMetadataRequest;
use dialoguer::Confirm;

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum MetadataCommand {
    /// List metadata on an instance
    #[command(visible_alias = "ls")]
    List(MetadataListArgs),

    /// Get a metadata value
    Get(MetadataGetArgs),

    /// Set metadata on an instance
    Set(MetadataSetArgs),

    /// Delete metadata from an instance
    #[command(visible_alias = "rm")]
    Delete(MetadataDeleteArgs),

    /// Delete all metadata from an instance
    DeleteAll(MetadataDeleteAllArgs),
}

#[derive(Args, Clone)]
pub struct MetadataListArgs {
    /// Instance ID or name
    pub instance: String,

    /// Include credentials (e.g., root_authorized_keys)
    #[arg(long)]
    pub credentials: bool,
}

#[derive(Args, Clone)]
pub struct MetadataGetArgs {
    /// Instance ID or name
    pub instance: String,

    /// Metadata key
    pub key: String,
}

#[derive(Args, Clone)]
pub struct MetadataSetArgs {
    /// Instance ID or name
    pub instance: String,

    /// Metadata to set (key=value, multiple allowed)
    #[arg(required_unless_present = "file")]
    pub metadata: Vec<String>,

    /// Read metadata from JSON file (use '-' for stdin)
    #[arg(short = 'f', long = "file")]
    pub file: Option<PathBuf>,

    /// Wait for metadata update to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,

    /// Suppress output after setting metadata
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,
}

#[derive(Args, Clone)]
pub struct MetadataDeleteArgs {
    /// Instance ID or name
    pub instance: String,

    /// Metadata key to delete
    pub key: String,

    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct MetadataDeleteAllArgs {
    /// Instance ID or name
    pub instance: String,

    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

impl MetadataCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_metadata(args, client, use_json).await,
            Self::Get(args) => get_metadata(args, client).await,
            Self::Set(args) => set_metadata(args, client).await,
            Self::Delete(args) => delete_metadata(args, client).await,
            Self::DeleteAll(args) => delete_all_metadata(args, client).await,
        }
    }
}

pub async fn list_metadata(
    args: MetadataListArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    // Note: credentials parameter is not currently supported in the API
    let _ = args.credentials; // silence unused warning

    let response = client
        .inner()
        .list_machine_metadata()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;
    let metadata = response.into_inner();

    if use_json {
        json::print_json(&metadata)?;
    } else {
        let mut tbl = table::create_table(&["KEY", "VALUE"]);
        // Metadata is a HashMap<String, String>
        for (key, value) in metadata.iter() {
            // Truncate long values for display
            let display_value = if value.len() > 60 {
                format!("{}...", &value[..57])
            } else {
                value.clone()
            };
            tbl.add_row(vec![key, &display_value]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_metadata(args: MetadataGetArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .get_machine_metadata()
        .account(account)
        .machine(&machine_id)
        .key(&args.key)
        .send()
        .await?;

    let value = response.into_inner();
    println!("{}", value);

    Ok(())
}

async fn set_metadata(args: MetadataSetArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    // Parse metadata from file or command line
    let meta_map: serde_json::Map<String, serde_json::Value> = if let Some(file_path) = &args.file {
        let content = if file_path.as_os_str() == "-" {
            use std::io::Read;
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        } else {
            std::fs::read_to_string(file_path)?
        };
        serde_json::from_str(&content)?
    } else {
        let mut map = serde_json::Map::new();
        for meta in &args.metadata {
            if let Some((key, value)) = meta.split_once('=') {
                map.insert(
                    key.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            } else {
                return Err(anyhow::anyhow!(
                    "Invalid metadata format '{}', expected key=value",
                    meta
                ));
            }
        }
        map
    };

    let request = AddMetadataRequest::from(meta_map.clone());

    client
        .inner()
        .add_machine_metadata()
        .account(account)
        .machine(&machine_id)
        .body(request)
        .send()
        .await?;

    if !args.quiet {
        for (key, _) in &meta_map {
            println!("Set metadata {}", key);
        }
    }

    if args.wait {
        super::wait::wait_for_state(&machine_id, "running", args.wait_timeout, client).await?;
        if !args.quiet {
            println!("Instance {} is running", &machine_id[..8]);
        }
    }

    Ok(())
}

async fn delete_metadata(args: MetadataDeleteArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt(format!("Delete metadata {}?", args.key))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    client
        .inner()
        .delete_machine_metadata()
        .account(account)
        .machine(&machine_id)
        .key(&args.key)
        .send()
        .await?;

    println!("Deleted metadata {}", args.key);

    Ok(())
}

async fn delete_all_metadata(args: MetadataDeleteAllArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt("Delete ALL metadata? This cannot be undone.")
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    client
        .inner()
        .delete_all_machine_metadata()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    println!("Deleted all metadata");

    Ok(())
}
