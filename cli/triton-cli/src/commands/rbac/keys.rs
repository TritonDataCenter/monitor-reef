// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC user key management commands

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

use crate::output::{json, table};

use super::common::resolve_user;

#[derive(Args, Clone)]
pub struct UserKeysArgs {
    /// User login or UUID
    pub user: String,
}

#[derive(Args, Clone)]
pub struct UserKeyGetArgs {
    /// User login or UUID
    pub user: String,
    /// Key name or fingerprint
    pub key: String,
}

#[derive(Args, Clone)]
pub struct UserKeyAddArgs {
    /// User login or UUID
    pub user: String,
    /// Key name
    #[arg(long, short)]
    pub name: String,
    /// SSH public key (or path to key file with @/path/to/key)
    #[arg(long, short)]
    pub key: String,
}

#[derive(Args, Clone)]
pub struct UserKeyDeleteArgs {
    /// User login or UUID
    pub user: String,
    /// Key name or fingerprint
    pub key: String,
    /// Skip confirmation
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

pub async fn list_user_keys(
    args: UserKeysArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let user_id = resolve_user(&args.user, client).await?;

    let response = client
        .inner()
        .list_user_keys()
        .account(account)
        .uuid(&user_id)
        .send()
        .await?;

    let keys = response.into_inner();

    if use_json {
        json::print_json(&keys)?;
    } else {
        let mut tbl = table::create_table(&["NAME", "FINGERPRINT"]);
        for key in &keys {
            tbl.add_row(vec![&key.name, &key.fingerprint]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

pub async fn get_user_key(
    args: UserKeyGetArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let user_id = resolve_user(&args.user, client).await?;

    let response = client
        .inner()
        .get_user_key()
        .account(account)
        .uuid(&user_id)
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

pub async fn add_user_key(
    args: UserKeyAddArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let user_id = resolve_user(&args.user, client).await?;

    // Read key from file if prefixed with @
    let key_data = if args.key.starts_with('@') {
        let path = &args.key[1..];
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read key file '{}': {}", path, e))?
            .trim()
            .to_string()
    } else {
        args.key.clone()
    };

    let request = cloudapi_client::types::CreateSshKeyRequest {
        name: args.name.clone(),
        key: key_data,
    };

    let response = client
        .inner()
        .create_user_key()
        .account(account)
        .uuid(&user_id)
        .body(request)
        .send()
        .await?;

    let key = response.into_inner();
    println!("Added key '{}' to user", key.name);

    if use_json {
        json::print_json(&key)?;
    }

    Ok(())
}

pub async fn delete_user_key(args: UserKeyDeleteArgs, client: &TypedClient) -> Result<()> {
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt(format!(
                "Delete key '{}' from user '{}'?",
                args.key, args.user
            ))
            .default(false)
            .interact()?
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    let account = &client.auth_config().account;
    let user_id = resolve_user(&args.user, client).await?;

    client
        .inner()
        .delete_user_key()
        .account(account)
        .uuid(&user_id)
        .name(&args.key)
        .send()
        .await?;

    println!("Deleted key '{}' from user '{}'", args.key, args.user);

    Ok(())
}
