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

/// User subcommands (modern pattern)
#[derive(Subcommand, Clone)]
pub enum UserSubcommand {
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

/// RBAC user command supporting both subcommands and action flags
///
/// This command supports two patterns for compatibility:
///
/// Modern (subcommand) pattern:
///   triton rbac user list
///   triton rbac user get USER
///   triton rbac user create LOGIN --email foo@bar.com
///   triton rbac user delete USER
///
/// Legacy (action flag) pattern:
///   triton rbac user USER           # show user (default)
///   triton rbac user -k USER        # show user with keys
///   triton rbac user -a [FILE]      # add user from file or stdin
///   triton rbac user -d USER...     # delete user(s)
#[derive(Args, Clone)]
#[command(args_conflicts_with_subcommands = true)]
pub struct RbacUserCommand {
    #[command(subcommand)]
    pub command: Option<UserSubcommand>,

    /// Add a new user (legacy compat: read from FILE, "-" for stdin, or interactive)
    #[arg(short = 'a', long = "add", conflicts_with = "delete")]
    pub add: bool,

    /// Delete user(s) (legacy compat)
    #[arg(short = 'd', long = "delete", conflicts_with = "add")]
    pub delete: bool,

    /// Include SSH keys when showing user
    #[arg(short = 'k', long = "keys")]
    pub keys: bool,

    /// Skip confirmation (for delete)
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// User(s) or file argument
    /// For show: USER login/uuid
    /// For add: optional FILE path (or "-" for stdin)
    /// For delete: one or more USER login/uuid
    #[arg(trailing_var_arg = true)]
    pub args: Vec<String>,
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
        // If a subcommand is provided, use the modern pattern
        if let Some(cmd) = self.command {
            return match cmd {
                UserSubcommand::List => list_users(client, use_json).await,
                UserSubcommand::Get(args) => get_user(args, client, use_json).await,
                UserSubcommand::Create(args) => create_user(args, client, use_json).await,
                UserSubcommand::Update(args) => update_user(args, client, use_json).await,
                UserSubcommand::Delete(args) => delete_users(args, client).await,
            };
        }

        // Legacy action flag pattern
        if self.add {
            // -a/--add: add user from file or stdin
            let file = self.args.first().map(|s| s.as_str());
            add_user_from_file(file, client, use_json).await
        } else if self.delete {
            // -d/--delete: delete user(s)
            if self.args.is_empty() {
                anyhow::bail!("USER argument(s) required for delete");
            }
            let args = UserDeleteArgs {
                users: self.args,
                force: self.yes,
            };
            delete_users(args, client).await
        } else if !self.args.is_empty() {
            // Default: show user
            let args = UserGetArgs {
                user: self.args[0].clone(),
                keys: self.keys,
            };
            get_user(args, client, use_json).await
        } else {
            // No args and no subcommand - show usage hint
            anyhow::bail!(
                "Usage: triton rbac user <SUBCOMMAND>\n\
                 Or:    triton rbac user USER           (show user)\n\
                 Or:    triton rbac user -a [FILE]      (add user)\n\
                 Or:    triton rbac user -d USER...     (delete users)\n\n\
                 Run 'triton rbac user --help' for more information"
            );
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

/// Add user from file (legacy -a flag support)
///
/// Reads user JSON from:
/// - A file path
/// - stdin (when file is "-")
/// - Interactive prompts (when file is None)
async fn add_user_from_file(
    file: Option<&str>,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    use std::io::{self, Read};

    // Read JSON input based on source
    let json_data: serde_json::Value = match file {
        Some("-") => {
            // Read from stdin
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer)?;
            serde_json::from_str(&buffer)
                .map_err(|e| anyhow::anyhow!("invalid JSON on stdin: {}", e))?
        }
        Some(path) => {
            // Read from file
            let content = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("failed to read file '{}': {}", path, e))?;
            serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("invalid JSON in '{}': {}", path, e))?
        }
        None => {
            // Interactive mode - prompt for fields
            use dialoguer::{Input, Password};

            let login: String = Input::new().with_prompt("Login").interact_text()?;

            let email: String = Input::new().with_prompt("Email").interact_text()?;

            let password = Password::new()
                .with_prompt("Password")
                .with_confirmation("Confirm password", "Passwords do not match")
                .interact()?;

            let first_name: String = Input::new()
                .with_prompt("First name (optional)")
                .allow_empty(true)
                .interact_text()?;

            let last_name: String = Input::new()
                .with_prompt("Last name (optional)")
                .allow_empty(true)
                .interact_text()?;

            let company_name: String = Input::new()
                .with_prompt("Company name (optional)")
                .allow_empty(true)
                .interact_text()?;

            serde_json::json!({
                "login": login,
                "email": email,
                "password": password,
                "firstName": if first_name.is_empty() { None } else { Some(first_name) },
                "lastName": if last_name.is_empty() { None } else { Some(last_name) },
                "companyName": if company_name.is_empty() { None } else { Some(company_name) },
            })
        }
    };

    // Extract required fields
    let login = json_data
        .get("login")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required field: login"))?
        .to_string();

    let email = json_data
        .get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required field: email"))?
        .to_string();

    let password = json_data
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required field: password"))?
        .to_string();

    // Extract optional fields (support both camelCase and snake_case)
    let first_name = json_data
        .get("firstName")
        .or_else(|| json_data.get("first_name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let last_name = json_data
        .get("lastName")
        .or_else(|| json_data.get("last_name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let company_name = json_data
        .get("companyName")
        .or_else(|| json_data.get("company_name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let phone = json_data
        .get("phone")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Create the user
    let account = &client.auth_config().account;
    let request = cloudapi_client::types::CreateUserRequest {
        login: login.clone(),
        email,
        password,
        company_name,
        first_name,
        last_name,
        phone,
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
