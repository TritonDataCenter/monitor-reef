// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance tag subcommands

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use cloudapi_client::types::TagsRequest;
use dialoguer::Confirm;
use serde_json::{Map, Value};

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum TagCommand {
    /// List tags on an instance
    #[command(alias = "ls")]
    List(TagListArgs),

    /// Get a tag value
    Get(TagGetArgs),

    /// Set tag(s) on an instance
    Set(TagSetArgs),

    /// Delete a tag from an instance
    #[command(alias = "rm")]
    Delete(TagDeleteArgs),

    /// Replace all tags on an instance
    Replace(TagReplaceArgs),
}

#[derive(Args, Clone)]
pub struct TagListArgs {
    /// Instance ID or name
    pub instance: String,
}

#[derive(Args, Clone)]
pub struct TagGetArgs {
    /// Instance ID or name
    pub instance: String,

    /// Tag key
    pub key: String,
}

#[derive(Args, Clone)]
pub struct TagSetArgs {
    /// Instance ID or name
    pub instance: String,

    /// Tags to set (key=value, multiple allowed)
    #[arg(required_unless_present = "file")]
    pub tags: Vec<String>,

    /// Read tags from JSON file (use '-' for stdin)
    #[arg(short = 'f', long = "file")]
    pub file: Option<PathBuf>,

    /// Wait for tag update to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,

    /// Suppress output after setting tags
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,
}

#[derive(Args, Clone)]
pub struct TagDeleteArgs {
    /// Instance ID or name
    pub instance: String,

    /// Tag key to delete
    pub key: String,

    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct TagReplaceArgs {
    /// Instance ID or name
    pub instance: String,

    /// Tags to set (key=value, multiple allowed)
    #[arg(required = true)]
    pub tags: Vec<String>,

    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

impl TagCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_tags(args, client, use_json).await,
            Self::Get(args) => get_tag(args, client).await,
            Self::Set(args) => set_tags(args, client).await,
            Self::Delete(args) => delete_tag(args, client).await,
            Self::Replace(args) => replace_tags(args, client).await,
        }
    }
}

pub async fn list_tags(args: TagListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .list_machine_tags()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let tags = response.into_inner();

    if use_json {
        json::print_json(&tags)?;
    } else {
        let mut tbl = table::create_table(&["KEY", "VALUE"]);
        // Tags is a HashMap<String, serde_json::Value>
        for (key, value) in tags.iter() {
            let value_str = match value {
                serde_json::Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            tbl.add_row(vec![key, &value_str]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_tag(args: TagGetArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .get_machine_tag()
        .account(account)
        .machine(&machine_id)
        .tag(&args.key)
        .send()
        .await?;

    let value = response.into_inner();
    println!("{}", value);

    Ok(())
}

async fn set_tags(args: TagSetArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    // Parse tags from file or command line
    let tag_map: Map<String, Value> = if let Some(file_path) = &args.file {
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
        let mut map: Map<String, Value> = Map::new();
        for tag in &args.tags {
            if let Some((key, value)) = tag.split_once('=') {
                map.insert(key.to_string(), Value::String(value.to_string()));
            } else {
                return Err(anyhow::anyhow!(
                    "Invalid tag format '{}', expected key=value",
                    tag
                ));
            }
        }
        map
    };

    let request = TagsRequest::from(tag_map.clone());

    client
        .inner()
        .add_machine_tags()
        .account(account)
        .machine(&machine_id)
        .body(request)
        .send()
        .await?;

    if !args.quiet {
        for (key, value) in &tag_map {
            let val_str = match value {
                Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            println!("Set tag {}={}", key, val_str);
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

async fn delete_tag(args: TagDeleteArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt(format!("Delete tag {}?", args.key))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    client
        .inner()
        .delete_machine_tag()
        .account(account)
        .machine(&machine_id)
        .tag(&args.key)
        .send()
        .await?;

    println!("Deleted tag {}", args.key);

    Ok(())
}

async fn replace_tags(args: TagReplaceArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt("Replace all tags? (existing tags will be removed)")
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let mut tag_map: Map<String, Value> = Map::new();
    for tag in &args.tags {
        if let Some((key, value)) = tag.split_once('=') {
            tag_map.insert(key.to_string(), Value::String(value.to_string()));
        } else {
            return Err(anyhow::anyhow!(
                "Invalid tag format '{}', expected key=value",
                tag
            ));
        }
    }

    let request = TagsRequest::from(tag_map.clone());

    client
        .inner()
        .replace_machine_tags()
        .account(account)
        .machine(&machine_id)
        .body(request)
        .send()
        .await?;

    println!("Replaced all tags");
    for (key, value) in &tag_map {
        let val_str = match value {
            Value::String(s) => s.clone(),
            _ => value.to_string(),
        };
        println!("  {}={}", key, val_str);
    }

    Ok(())
}
