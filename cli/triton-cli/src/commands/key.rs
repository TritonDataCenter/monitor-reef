// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! SSH key management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct KeyListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Subcommand, Clone)]
pub enum KeyCommand {
    /// List SSH keys
    #[command(alias = "ls")]
    List(KeyListArgs),
    /// Get SSH key details
    Get(KeyGetArgs),
    /// Add SSH key
    Add(KeyAddArgs),
    /// Delete SSH key(s)
    #[command(alias = "rm")]
    Delete(KeyDeleteArgs),
}

#[derive(Args, Clone)]
pub struct KeyGetArgs {
    /// Key name or fingerprint
    pub key: String,
}

#[derive(Args, Clone)]
pub struct KeyAddArgs {
    /// Key name
    #[arg(short, long)]
    pub name: Option<String>,
    /// Key file path (or read from stdin if not provided)
    pub file: Option<String>,
}

#[derive(Args, Clone)]
pub struct KeyDeleteArgs {
    /// Key name(s) or fingerprint(s)
    pub keys: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

impl KeyCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_keys(args, client, use_json).await,
            Self::Get(args) => get_key(args, client, use_json).await,
            Self::Add(args) => add_key(args, client, use_json).await,
            Self::Delete(args) => delete_keys(args, client).await,
        }
    }
}

async fn list_keys(args: KeyListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client.inner().list_keys().account(account).send().await?;

    let keys = response.into_inner();

    if use_json {
        json::print_json(&keys)?;
    } else {
        let mut tbl = TableBuilder::new(&["NAME", "FINGERPRINT"]).with_long_headers(&["KEY"]);
        for key in &keys {
            tbl.add_row(vec![
                key.name.clone(),
                key.fingerprint.clone(),
                key.key.clone(),
            ]);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

async fn get_key(args: KeyGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .get_key()
        .account(account)
        .name(&args.key)
        .send()
        .await?;

    let key = response.into_inner();

    if use_json {
        json::print_json(&key)?;
    } else {
        println!("Name:        {}", key.name);
        println!("Fingerprint: {}", key.fingerprint);
        println!("Key:         {}", key.key);
    }

    Ok(())
}

async fn add_key(args: KeyAddArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // Read key from file or stdin
    let key_content = if let Some(file) = &args.file {
        std::fs::read_to_string(file)?
    } else {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    };

    // Extract name from key comment if not provided
    let name = args.name.unwrap_or_else(|| {
        key_content
            .split_whitespace()
            .last()
            .unwrap_or("key")
            .to_string()
    });

    let request = cloudapi_client::types::CreateSshKeyRequest {
        name: name.clone(),
        key: key_content.trim().to_string(),
    };

    let response = client
        .inner()
        .create_key()
        .account(account)
        .body(request)
        .send()
        .await?;

    let key = response.into_inner();
    println!("Added key '{}' ({})", key.name, key.fingerprint);

    if use_json {
        json::print_json(&key)?;
    }

    Ok(())
}

async fn delete_keys(args: KeyDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for key_name in &args.keys {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete key '{}'?", key_name))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        client
            .inner()
            .delete_key()
            .account(account)
            .name(key_name)
            .send()
            .await?;

        println!("Deleted key '{}'", key_name);
    }

    Ok(())
}
