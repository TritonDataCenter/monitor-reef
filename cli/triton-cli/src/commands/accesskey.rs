// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Access key management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::AccessKeyStatus;

use crate::output::enum_to_display;
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Subcommand, Clone)]
pub enum AccesskeyCommand {
    /// List access keys
    #[command(visible_alias = "ls")]
    List(AccesskeyListArgs),
    /// Get access key details
    Get(AccesskeyGetArgs),
    /// Create a new access key
    Create(AccesskeyCreateArgs),
    /// Update an access key
    Update(AccesskeyUpdateArgs),
    /// Delete access key(s)
    #[command(visible_alias = "rm")]
    Delete(AccesskeyDeleteArgs),
}

#[derive(Args, Clone)]
pub struct AccesskeyListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Args, Clone)]
pub struct AccesskeyGetArgs {
    /// Access key ID
    pub accesskeyid: String,
}

#[derive(Args, Clone)]
pub struct AccesskeyCreateArgs {
    /// Initial status (Active or Inactive)
    #[arg(short, long, ignore_case = true)]
    pub status: Option<AccessKeyStatus>,
    /// Description for the access key
    #[arg(short, long, visible_alias = "desc")]
    pub description: Option<String>,
}

#[derive(Args, Clone)]
pub struct AccesskeyUpdateArgs {
    /// Access key ID
    pub accesskeyid: String,
    /// New status (Active or Inactive)
    #[arg(short, long, ignore_case = true)]
    pub status: Option<AccessKeyStatus>,
    /// New description
    #[arg(short, long, visible_alias = "desc")]
    pub description: Option<String>,
    /// Read update data from JSON file (use '-' for stdin)
    #[arg(short = 'f', long = "file")]
    pub file: Option<std::path::PathBuf>,
}

#[derive(Args, Clone)]
pub struct AccesskeyDeleteArgs {
    /// Access key ID(s)
    pub accesskeyids: Vec<String>,
    /// Skip confirmation
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

impl AccesskeyCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_access_keys(args, client, use_json).await,
            Self::Get(args) => get_access_key(args, client, use_json).await,
            Self::Create(args) => create_access_key(args, client, use_json).await,
            Self::Update(args) => update_access_key(args, client, use_json).await,
            Self::Delete(args) => delete_access_keys(args, client).await,
        }
    }
}

async fn list_access_keys(
    args: AccesskeyListArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let response = client
        .inner()
        .list_access_keys()
        .account(account)
        .send()
        .await?;

    let mut keys = response.into_inner();
    // Sort by created timestamp to match node-triton behavior
    keys.sort_by(|a, b| a.created.cmp(&b.created));

    if use_json {
        json::print_json_stream(&keys)?;
    } else {
        let mut tbl = TableBuilder::new(&["ACCESSKEYID", "STATUS", "CREDENTIALTYPE", "UPDATED"])
            .with_long_headers(&["DESCRIPTION", "CREATED", "EXPIRATION"]);
        for key in &keys {
            tbl.add_row(vec![
                key.accesskeyid.clone(),
                enum_to_display(&key.status),
                enum_to_display(&key.credentialtype),
                key.updated.to_rfc3339(),
                key.description.clone().unwrap_or_default(),
                key.created.to_rfc3339(),
                key.expiration.map(|e| e.to_rfc3339()).unwrap_or_default(),
            ]);
        }
        tbl.print(&args.table)?;
    }

    Ok(())
}

async fn get_access_key(
    args: AccesskeyGetArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let response = client
        .inner()
        .get_access_key()
        .account(account)
        .accesskeyid(&args.accesskeyid)
        .send()
        .await?;

    let key = response.into_inner();

    if use_json {
        json::print_json(&key)?;
    } else {
        json::print_json_pretty(&key)?;
    }

    Ok(())
}

async fn create_access_key(
    args: AccesskeyCreateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();

    let request = triton_gateway_client::types::CreateAccessKeyRequest {
        status: args.status,
        description: args.description,
    };

    let response = client
        .inner()
        .create_access_key()
        .account(account)
        .body(request)
        .send()
        .await?;

    let key = response.into_inner();

    if use_json {
        json::print_json(&key)?;
    } else {
        println!("AccessKeyId:     {}", key.accesskeyid);
        println!("AccessKeySecret: {}", key.accesskeysecret);
        println!();
        println!("WARNING: Save the secret now. It cannot be retrieved again.");
    }

    Ok(())
}

async fn update_access_key(
    args: AccesskeyUpdateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();

    let request = if let Some(file_path) = &args.file {
        let content = if file_path.as_os_str() == "-" {
            use std::io::Read;
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        } else {
            tokio::fs::read_to_string(file_path).await?
        };
        serde_json::from_str(&content)?
    } else {
        triton_gateway_client::types::UpdateAccessKeyRequest {
            status: args.status,
            description: args.description,
        }
    };

    let response = client
        .inner()
        .update_access_key()
        .account(account)
        .accesskeyid(&args.accesskeyid)
        .body(request)
        .send()
        .await?;

    let key = response.into_inner();

    if use_json {
        json::print_json(&key)?;
    } else {
        println!("Updated access key {}", key.accesskeyid);
        json::print_json_pretty(&key)?;
    }

    Ok(())
}

async fn delete_access_keys(args: AccesskeyDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = client.effective_account();

    for id in &args.accesskeyids {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete access key '{}'?", id))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        client
            .inner()
            .delete_access_key()
            .account(account)
            .accesskeyid(id)
            .send()
            .await?;

        println!("Deleted access key '{}'", id);
    }

    Ok(())
}
