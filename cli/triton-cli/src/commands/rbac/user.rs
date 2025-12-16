// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC user management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{json, table};

use super::common::resolve_user;

#[derive(Subcommand, Clone)]
pub enum RbacUserCommand {
    /// List RBAC users
    #[command(alias = "ls")]
    List,
    /// Get user details
    Get(UserGetArgs),
    /// Create user
    Create(UserCreateArgs),
    /// Update user
    Update(UserUpdateArgs),
    /// Delete user(s)
    #[command(alias = "rm")]
    Delete(UserDeleteArgs),
}

#[derive(Args, Clone)]
pub struct UserGetArgs {
    /// User login or UUID
    pub user: String,
    /// Include SSH keys for the user
    #[arg(long, short)]
    pub keys: bool,
}

#[derive(Args, Clone)]
pub struct UserCreateArgs {
    /// User login
    pub login: String,
    /// Email address
    #[arg(long)]
    pub email: String,
    /// Password (will prompt if not provided)
    #[arg(long)]
    pub password: Option<String>,
    /// Company name
    #[arg(long)]
    pub company_name: Option<String>,
    /// First name
    #[arg(long)]
    pub first_name: Option<String>,
    /// Last name
    #[arg(long)]
    pub last_name: Option<String>,
}

#[derive(Args, Clone)]
pub struct UserUpdateArgs {
    /// User login or UUID
    pub user: String,
    /// New email
    #[arg(long)]
    pub email: Option<String>,
    /// Company name
    #[arg(long)]
    pub company_name: Option<String>,
    /// First name
    #[arg(long)]
    pub first_name: Option<String>,
    /// Last name
    #[arg(long)]
    pub last_name: Option<String>,
}

#[derive(Args, Clone)]
pub struct UserDeleteArgs {
    /// User login(s) or UUID(s)
    pub users: Vec<String>,
    /// Skip confirmation
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

impl RbacUserCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_users(client, use_json).await,
            Self::Get(args) => get_user(args, client, use_json).await,
            Self::Create(args) => create_user(args, client, use_json).await,
            Self::Update(args) => update_user(args, client, use_json).await,
            Self::Delete(args) => delete_users(args, client).await,
        }
    }
}

pub async fn list_users(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client.inner().list_users().account(account).send().await?;

    let users = response.into_inner();

    if use_json {
        json::print_json(&users)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "LOGIN", "EMAIL", "NAME"]);
        for user in &users {
            let name = match (&user.first_name, &user.last_name) {
                (Some(f), Some(l)) => format!("{} {}", f, l),
                (Some(f), None) => f.clone(),
                (None, Some(l)) => l.clone(),
                (None, None) => "-".to_string(),
            };
            tbl.add_row(vec![
                &user.id.to_string()[..8],
                &user.login,
                &user.email,
                &name,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_user(args: UserGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let user_id = resolve_user(&args.user, client).await?;

    let response = client
        .inner()
        .get_user()
        .account(account)
        .uuid(&user_id)
        .send()
        .await?;

    let user = response.into_inner();

    // Optionally fetch keys if -k/--keys flag is set
    let keys = if args.keys {
        let keys_response = client
            .inner()
            .list_user_keys()
            .account(account)
            .uuid(&user_id)
            .send()
            .await?;
        Some(keys_response.into_inner())
    } else {
        None
    };

    if use_json {
        if let Some(keys) = keys {
            // Combine user and keys into a single JSON object
            let mut combined = serde_json::to_value(&user)?;
            if let serde_json::Value::Object(ref mut map) = combined {
                map.insert("keys".to_string(), serde_json::to_value(&keys)?);
            }
            json::print_json(&combined)?;
        } else {
            json::print_json(&user)?;
        }
    } else {
        println!("ID:         {}", user.id);
        println!("Login:      {}", user.login);
        println!("Email:      {}", user.email);
        if let Some(f) = &user.first_name {
            print!("Name:       {}", f);
            if let Some(l) = &user.last_name {
                print!(" {}", l);
            }
            println!();
        }
        if let Some(c) = &user.company_name {
            println!("Company:    {}", c);
        }
        if let Some(p) = &user.phone {
            println!("Phone:      {}", p);
        }
        println!("Created:    {}", user.created);
        println!("Updated:    {}", user.updated);

        // Display keys if fetched
        if let Some(keys) = keys {
            println!("Keys:");
            if keys.is_empty() {
                println!("  (no keys)");
            } else {
                for key in &keys {
                    println!("  - {} ({})", key.name, key.fingerprint);
                }
            }
        }
    }

    Ok(())
}

async fn create_user(args: UserCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // Prompt for password if not provided
    let password = match args.password {
        Some(p) => p,
        None => {
            use dialoguer::Password;
            Password::new()
                .with_prompt("Password")
                .with_confirmation("Confirm password", "Passwords do not match")
                .interact()?
        }
    };

    let request = cloudapi_client::types::CreateUserRequest {
        login: args.login.clone(),
        email: args.email,
        password,
        company_name: args.company_name,
        first_name: args.first_name,
        last_name: args.last_name,
        phone: None,
    };

    let response = client
        .inner()
        .create_user()
        .account(account)
        .body(request)
        .send()
        .await?;

    let user = response.into_inner();
    println!("Created user '{}' ({})", user.login, user.id);

    if use_json {
        json::print_json(&user)?;
    }

    Ok(())
}

async fn update_user(args: UserUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let user_id = resolve_user(&args.user, client).await?;

    let request = cloudapi_client::types::UpdateUserRequest {
        email: args.email,
        company_name: args.company_name,
        first_name: args.first_name,
        last_name: args.last_name,
        phone: None,
    };

    let response = client
        .inner()
        .update_user()
        .account(account)
        .uuid(&user_id)
        .body(request)
        .send()
        .await?;

    let user = response.into_inner();
    println!("Updated user '{}'", user.login);

    if use_json {
        json::print_json(&user)?;
    }

    Ok(())
}

pub async fn delete_users(args: UserDeleteArgs, client: &TypedClient) -> Result<()> {
    for user_ref in &args.users {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete user '{}'?", user_ref))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        let user_id = resolve_user(user_ref, client).await?;
        let account = &client.auth_config().account;

        client
            .inner()
            .delete_user()
            .account(account)
            .uuid(&user_id)
            .send()
            .await?;

        println!("Deleted user '{}'", user_ref);
    }

    Ok(())
}
