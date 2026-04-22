// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! RBAC user key management commands

use anyhow::Result;
use clap::Args;
use cloudapi_api::SshKey;

use crate::client::AnyClient;
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};
use crate::{dispatch, dispatch_with_types};

use super::common::resolve_user;

/// RBAC key command supporting action flags for node-triton compatibility
///
/// This command supports:
///   triton rbac key USER KEY           # show key (default)
///   triton rbac key -a [-n NAME] USER FILE  # add key from file
///   triton rbac key -d USER KEY...     # delete key(s)
#[derive(Args, Clone)]
pub struct RbacKeyCommand {
    /// Add a new key (legacy compat)
    #[arg(short = 'a', long = "add", conflicts_with = "delete")]
    pub add: bool,

    /// Delete key(s) (legacy compat)
    #[arg(short = 'd', long = "delete", conflicts_with = "add")]
    pub delete: bool,

    /// Key name (for add)
    #[arg(short = 'n', long = "name")]
    pub name: Option<String>,

    /// Skip confirmation (for delete)
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Arguments: USER KEY (for show), USER FILE (for add), USER KEY... (for delete)
    pub args: Vec<String>,
}

impl RbacKeyCommand {
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
        if self.add {
            // -a/--add: add key from file
            if self.args.len() < 2 {
                anyhow::bail!("Usage: triton rbac key -a [-n NAME] USER FILE");
            }
            let user = &self.args[0];
            let file = &self.args[1];
            add_key_from_file(user, file, self.name, client, use_json).await
        } else if self.delete {
            // -d/--delete: delete key(s)
            if self.args.len() < 2 {
                anyhow::bail!("Usage: triton rbac key -d USER KEY...");
            }
            let user = &self.args[0];
            let keys: Vec<String> = self.args[1..].to_vec();
            delete_keys(user, keys, self.yes, client).await
        } else if self.args.len() >= 2 {
            // Default: show key
            let args = UserKeyGetArgs {
                user: self.args[0].clone(),
                key: self.args[1].clone(),
            };
            get_user_key(args, client, use_json).await
        } else {
            anyhow::bail!(
                "Usage: triton rbac key USER KEY           (show key)\n\
                 Or:    triton rbac key -a [-n NAME] USER FILE  (add key)\n\
                 Or:    triton rbac key -d USER KEY...     (delete keys)\n\n\
                 Run 'triton rbac key --help' for more information"
            );
        }
    }
}

#[derive(Args, Clone)]
pub struct UserKeysArgs {
    /// User login or UUID
    pub user: String,

    #[command(flatten)]
    pub table: TableFormatArgs,
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

pub async fn list_user_keys(args: UserKeysArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(&args.user, client).await?;

    let keys: Vec<SshKey> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_user_keys()
            .account(account)
            .uuid(&user_id)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Vec<SshKey>>(serde_json::to_value(&resp)?)?
    });

    if use_json {
        json::print_json_stream(&keys)?;
    } else {
        let mut tbl = TableBuilder::new(&["NAME", "FINGERPRINT"]).with_long_headers(&["KEY"]);
        for key in &keys {
            tbl.add_row(vec![
                key.name.clone(),
                key.fingerprint.clone(),
                key.key.clone(),
            ]);
        }
        tbl.print(&args.table)?;
    }

    Ok(())
}

pub async fn get_user_key(args: UserKeyGetArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(&args.user, client).await?;

    let key: SshKey = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_user_key()
            .account(account)
            .uuid(&user_id)
            .name(&args.key)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<SshKey>(serde_json::to_value(&resp)?)?
    });

    if use_json {
        json::print_json(&key)?;
    } else {
        println!("Name:        {}", key.name);
        println!("Fingerprint: {}", key.fingerprint);
        println!("Key:         {}", key.key);
    }

    Ok(())
}

pub async fn add_user_key(args: UserKeyAddArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(&args.user, client).await?;

    // Read key from file if prefixed with @
    let key_data = if args.key.starts_with('@') {
        let path = &args.key[1..];
        tokio::fs::read_to_string(path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read key file '{}': {}", path, e))?
    } else {
        args.key.clone()
    };

    let name = args.name.clone();
    let key: SshKey = dispatch_with_types!(client, |c, t| {
        let request = t::CreateSshKeyRequest {
            name: name.clone(),
            key: key_data.clone(),
        };
        let resp = c
            .inner()
            .create_user_key()
            .account(account)
            .uuid(&user_id)
            .body(request)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<SshKey>(serde_json::to_value(&resp)?)?
    });

    println!("Added key '{}' to user", key.name);

    if use_json {
        json::print_json(&key)?;
    }

    Ok(())
}

pub async fn delete_user_key(args: UserKeyDeleteArgs, client: &AnyClient) -> Result<()> {
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

    let account = client.effective_account();
    let user_id = resolve_user(&args.user, client).await?;

    dispatch!(client, |c| {
        c.inner()
            .delete_user_key()
            .account(account)
            .uuid(&user_id)
            .name(&args.key)
            .send()
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    println!("Deleted key '{}' from user '{}'", args.key, args.user);

    Ok(())
}

/// Add key from file (legacy -a flag support)
async fn add_key_from_file(
    user: &str,
    file: &str,
    name: Option<String>,
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    use std::io::{self, Read};

    let account = client.effective_account();
    let user_id = resolve_user(user, client).await?;

    // Read key from file or stdin
    let key_data = if file == "-" {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else {
        tokio::fs::read_to_string(file)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read key file '{}': {}", file, e))?
    };

    // Extract name from key comment if not provided
    let key_name = match name {
        Some(n) => n,
        None => {
            // Try to extract comment from SSH key (last part after spaces)
            key_data
                .split_whitespace()
                .nth(2)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "imported-key".to_string())
        }
    };

    let key: SshKey = dispatch_with_types!(client, |c, t| {
        let request = t::CreateSshKeyRequest {
            name: key_name.clone(),
            key: key_data.clone(),
        };
        let resp = c
            .inner()
            .create_user_key()
            .account(account)
            .uuid(&user_id)
            .body(request)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<SshKey>(serde_json::to_value(&resp)?)?
    });

    println!(
        "Added user {} key \"{}\"{}",
        user,
        key.fingerprint,
        if !key_name.is_empty() {
            format!(" ({})", key.name)
        } else {
            String::new()
        }
    );

    if use_json {
        json::print_json(&key)?;
    }

    Ok(())
}

/// Delete multiple keys (legacy -d flag support)
async fn delete_keys(user: &str, keys: Vec<String>, yes: bool, client: &AnyClient) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(user, client).await?;

    for key in &keys {
        if !yes {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete user {} key \"{}\"?", user, key))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        dispatch!(client, |c| {
            c.inner()
                .delete_user_key()
                .account(account)
                .uuid(&user_id)
                .name(key)
                .send()
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;

        println!("Deleted user {} key \"{}\"", user, key);
    }

    Ok(())
}
