// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Access key management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_api::{AccessKey, CreateAccessKeyResponse};
use cloudapi_client::types::AccessKeyStatus;

use crate::client::AnyClient;
use crate::output::enum_to_display;
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};
use crate::{dispatch, dispatch_with_types};

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
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
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
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();

    let mut keys: Vec<AccessKey> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_access_keys()
            .account(account)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Vec<AccessKey>>(serde_json::to_value(&resp)?)?
    });

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
                key.updated.clone(),
                key.description.clone().unwrap_or_default(),
                key.created.clone(),
                key.expiration.clone().unwrap_or_default(),
            ]);
        }
        tbl.print(&args.table)?;
    }

    Ok(())
}

async fn get_access_key(args: AccesskeyGetArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();

    let key_json: serde_json::Value = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_access_key()
            .account(account)
            .accesskeyid(&args.accesskeyid)
            .send()
            .await?
            .into_inner();
        serde_json::to_value(&resp)?
    });

    if use_json {
        json::print_json(&key_json)?;
    } else {
        json::print_json_pretty(&key_json)?;
    }

    Ok(())
}

async fn create_access_key(
    args: AccesskeyCreateArgs,
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    // Serialize the AccessKeyStatus (if any) once, as a wire string
    // (`"Active"` / `"Inactive"`), so each arm can parse into its own
    // per-client enum.
    let status_str = args
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok())
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    let description = args.description.clone();

    let key: CreateAccessKeyResponse = dispatch_with_types!(client, |c, t| {
        let status: Option<t::AccessKeyStatus> = status_str
            .as_ref()
            .map(|s| serde_json::from_value(serde_json::Value::String(s.clone())))
            .transpose()?;
        let request = t::CreateAccessKeyRequest {
            status,
            description: description.clone(),
        };
        let resp = c
            .inner()
            .create_access_key()
            .account(account)
            .body(request)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<CreateAccessKeyResponse>(serde_json::to_value(&resp)?)?
    });

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
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();

    // Build the update-body as a JSON Value first (either read from file
    // or assembled from CLI flags). The per-client `UpdateAccessKeyRequest`
    // is serde-compatible on both sides, so each arm deserializes from the
    // same Value into its own typed struct.
    let body_value: serde_json::Value = if let Some(file_path) = &args.file {
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
        let mut obj = serde_json::Map::new();
        if let Some(s) = &args.status {
            obj.insert("status".into(), serde_json::to_value(s)?);
        }
        if let Some(d) = &args.description {
            obj.insert("description".into(), serde_json::Value::String(d.clone()));
        }
        serde_json::Value::Object(obj)
    };

    let key_json: serde_json::Value = dispatch_with_types!(client, |c, t| {
        let request: t::UpdateAccessKeyRequest = serde_json::from_value(body_value.clone())?;
        let resp = c
            .inner()
            .update_access_key()
            .account(account)
            .accesskeyid(&args.accesskeyid)
            .body(request)
            .send()
            .await?
            .into_inner();
        serde_json::to_value(&resp)?
    });

    if use_json {
        json::print_json(&key_json)?;
    } else {
        if let Some(id) = key_json.get("accesskeyid").and_then(|v| v.as_str()) {
            println!("Updated access key {}", id);
        }
        json::print_json_pretty(&key_json)?;
    }

    Ok(())
}

async fn delete_access_keys(args: AccesskeyDeleteArgs, client: &AnyClient) -> Result<()> {
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

        dispatch!(client, |c| {
            c.inner()
                .delete_access_key()
                .account(account)
                .accesskeyid(id)
                .send()
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;

        println!("Deleted access key '{}'", id);
    }

    Ok(())
}
