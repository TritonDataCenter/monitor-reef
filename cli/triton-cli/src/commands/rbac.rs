// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC (Role-Based Access Control) management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum RbacCommand {
    /// Show RBAC summary information
    Info,
    /// Apply RBAC configuration from a file
    Apply(ApplyArgs),
    /// Reset (delete) all RBAC users, roles, and policies
    Reset(ResetArgs),
    /// Manage RBAC users
    User {
        #[command(subcommand)]
        command: RbacUserCommand,
    },
    /// Manage RBAC roles
    Role {
        #[command(subcommand)]
        command: RbacRoleCommand,
    },
    /// Manage RBAC policies
    Policy {
        #[command(subcommand)]
        command: RbacPolicyCommand,
    },
    /// List SSH keys for a sub-user
    Keys(UserKeysArgs),
    /// Get SSH key for a sub-user
    Key(UserKeyGetArgs),
    /// Add SSH key to a sub-user
    #[command(alias = "add-key")]
    KeyAdd(UserKeyAddArgs),
    /// Delete SSH key from a sub-user
    #[command(alias = "delete-key", alias = "rm-key")]
    KeyDelete(UserKeyDeleteArgs),
}

// =============================================================================
// User Commands
// =============================================================================

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
    #[arg(long, short)]
    pub force: bool,
}

// =============================================================================
// Apply/Reset Commands
// =============================================================================

#[derive(Args, Clone)]
pub struct ApplyArgs {
    /// Path to RBAC configuration file (JSON format)
    pub file: PathBuf,
    /// Show what would be done without making changes
    #[arg(long, short = 'n')]
    pub dry_run: bool,
    /// Skip confirmation prompts
    #[arg(long, short)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct ResetArgs {
    /// Skip confirmation prompt
    #[arg(long, short)]
    pub force: bool,
}

/// RBAC configuration file format
#[derive(Debug, Deserialize)]
struct RbacConfig {
    #[serde(default)]
    users: Vec<RbacConfigUser>,
    #[serde(default)]
    roles: Vec<RbacConfigRole>,
    #[serde(default)]
    policies: Vec<RbacConfigPolicy>,
}

#[derive(Debug, Deserialize)]
struct RbacConfigUser {
    login: String,
    email: String,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    company_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RbacConfigRole {
    name: String,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    default_members: Vec<String>,
    #[serde(default)]
    policies: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RbacConfigPolicy {
    name: String,
    #[serde(default)]
    description: Option<String>,
    rules: Vec<String>,
}

// =============================================================================
// User Key Commands
// =============================================================================

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
    #[arg(long, short)]
    pub force: bool,
}

// =============================================================================
// Role Commands
// =============================================================================

#[derive(Subcommand, Clone)]
pub enum RbacRoleCommand {
    /// List RBAC roles
    #[command(alias = "ls")]
    List,
    /// Get role details
    Get(RoleGetArgs),
    /// Create role
    Create(RoleCreateArgs),
    /// Update role
    Update(RoleUpdateArgs),
    /// Delete role(s)
    #[command(alias = "rm")]
    Delete(RoleDeleteArgs),
}

#[derive(Args, Clone)]
pub struct RoleGetArgs {
    /// Role name or UUID
    pub role: String,
}

#[derive(Args, Clone)]
pub struct RoleCreateArgs {
    /// Role name
    pub name: String,
    /// Policies to attach (can be specified multiple times)
    #[arg(long)]
    pub policy: Vec<String>,
    /// Members (user logins, can be specified multiple times)
    #[arg(long)]
    pub member: Vec<String>,
    /// Default members (user logins, can be specified multiple times)
    #[arg(long)]
    pub default_member: Vec<String>,
}

#[derive(Args, Clone)]
pub struct RoleUpdateArgs {
    /// Role name or UUID
    pub role: String,
    /// New name
    #[arg(long)]
    pub name: Option<String>,
    /// Policies (replaces existing)
    #[arg(long)]
    pub policy: Vec<String>,
    /// Members (replaces existing)
    #[arg(long)]
    pub member: Vec<String>,
    /// Default members (replaces existing)
    #[arg(long)]
    pub default_member: Vec<String>,
}

#[derive(Args, Clone)]
pub struct RoleDeleteArgs {
    /// Role name(s) or UUID(s)
    pub roles: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

// =============================================================================
// Policy Commands
// =============================================================================

#[derive(Subcommand, Clone)]
pub enum RbacPolicyCommand {
    /// List RBAC policies
    #[command(alias = "ls")]
    List,
    /// Get policy details
    Get(PolicyGetArgs),
    /// Create policy
    Create(PolicyCreateArgs),
    /// Update policy
    Update(PolicyUpdateArgs),
    /// Delete policy(s)
    #[command(alias = "rm")]
    Delete(PolicyDeleteArgs),
}

#[derive(Args, Clone)]
pub struct PolicyGetArgs {
    /// Policy name or UUID
    pub policy: String,
}

#[derive(Args, Clone)]
pub struct PolicyCreateArgs {
    /// Policy name
    pub name: String,
    /// Policy rules (can be specified multiple times)
    #[arg(long, short)]
    pub rule: Vec<String>,
    /// Description
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Args, Clone)]
pub struct PolicyUpdateArgs {
    /// Policy name or UUID
    pub policy: String,
    /// New name
    #[arg(long)]
    pub name: Option<String>,
    /// New rules (replaces existing)
    #[arg(long, short)]
    pub rule: Vec<String>,
    /// New description
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Args, Clone)]
pub struct PolicyDeleteArgs {
    /// Policy name(s) or UUID(s)
    pub policies: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

// =============================================================================
// Implementation
// =============================================================================

impl RbacCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Info => rbac_info(client, use_json).await,
            Self::Apply(args) => rbac_apply(args, client, use_json).await,
            Self::Reset(args) => rbac_reset(args, client).await,
            Self::User { command } => command.run(client, use_json).await,
            Self::Role { command } => command.run(client, use_json).await,
            Self::Policy { command } => command.run(client, use_json).await,
            Self::Keys(args) => list_user_keys(args, client, use_json).await,
            Self::Key(args) => get_user_key(args, client, use_json).await,
            Self::KeyAdd(args) => add_user_key(args, client, use_json).await,
            Self::KeyDelete(args) => delete_user_key(args, client).await,
        }
    }
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

impl RbacRoleCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_roles(client, use_json).await,
            Self::Get(args) => get_role(args, client, use_json).await,
            Self::Create(args) => create_role(args, client, use_json).await,
            Self::Update(args) => update_role(args, client, use_json).await,
            Self::Delete(args) => delete_roles(args, client).await,
        }
    }
}

impl RbacPolicyCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_policies(client, use_json).await,
            Self::Get(args) => get_policy(args, client, use_json).await,
            Self::Create(args) => create_policy(args, client, use_json).await,
            Self::Update(args) => update_policy(args, client, use_json).await,
            Self::Delete(args) => delete_policies(args, client).await,
        }
    }
}

// =============================================================================
// User Implementation
// =============================================================================

async fn list_users(client: &TypedClient, use_json: bool) -> Result<()> {
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

    if use_json {
        json::print_json(&user)?;
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

async fn delete_users(args: UserDeleteArgs, client: &TypedClient) -> Result<()> {
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

async fn resolve_user(id_or_login: &str, client: &TypedClient) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_login).is_ok() {
        return Ok(id_or_login.to_string());
    }

    let account = &client.auth_config().account;
    let response = client.inner().list_users().account(account).send().await?;

    let users = response.into_inner();

    for user in &users {
        if user.login == id_or_login {
            return Ok(user.id.to_string());
        }
    }

    Err(anyhow::anyhow!("User not found: {}", id_or_login))
}

// =============================================================================
// User Key Implementation
// =============================================================================

async fn list_user_keys(args: UserKeysArgs, client: &TypedClient, use_json: bool) -> Result<()> {
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

async fn get_user_key(args: UserKeyGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
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

async fn add_user_key(args: UserKeyAddArgs, client: &TypedClient, use_json: bool) -> Result<()> {
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

async fn delete_user_key(args: UserKeyDeleteArgs, client: &TypedClient) -> Result<()> {
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

// =============================================================================
// Role Implementation
// =============================================================================

async fn list_roles(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client.inner().list_roles().account(account).send().await?;

    let roles = response.into_inner();

    if use_json {
        json::print_json(&roles)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "POLICIES", "MEMBERS"]);
        for role in &roles {
            tbl.add_row(vec![
                &role.id.to_string()[..8],
                &role.name,
                &role.policies.join(", "),
                &role.members.join(", "),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_role(args: RoleGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .get_role()
        .account(account)
        .role(&args.role)
        .send()
        .await?;

    let role = response.into_inner();

    if use_json {
        json::print_json(&role)?;
    } else {
        println!("ID:              {}", role.id);
        println!("Name:            {}", role.name);
        println!(
            "Policies:        {}",
            if role.policies.is_empty() {
                "-".to_string()
            } else {
                role.policies.join(", ")
            }
        );
        println!(
            "Members:         {}",
            if role.members.is_empty() {
                "-".to_string()
            } else {
                role.members.join(", ")
            }
        );
        println!(
            "Default members: {}",
            if role.default_members.is_empty() {
                "-".to_string()
            } else {
                role.default_members.join(", ")
            }
        );
    }

    Ok(())
}

async fn create_role(args: RoleCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::CreateRoleRequest {
        name: args.name.clone(),
        policies: if args.policy.is_empty() {
            None
        } else {
            Some(args.policy)
        },
        members: if args.member.is_empty() {
            None
        } else {
            Some(args.member)
        },
        default_members: if args.default_member.is_empty() {
            None
        } else {
            Some(args.default_member)
        },
    };

    let response = client
        .inner()
        .create_role()
        .account(account)
        .body(request)
        .send()
        .await?;

    let role = response.into_inner();
    println!("Created role '{}' ({})", role.name, role.id);

    if use_json {
        json::print_json(&role)?;
    }

    Ok(())
}

async fn update_role(args: RoleUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::UpdateRoleRequest {
        name: args.name,
        policies: if args.policy.is_empty() {
            None
        } else {
            Some(args.policy)
        },
        members: if args.member.is_empty() {
            None
        } else {
            Some(args.member)
        },
        default_members: if args.default_member.is_empty() {
            None
        } else {
            Some(args.default_member)
        },
    };

    let response = client
        .inner()
        .update_role()
        .account(account)
        .role(&args.role)
        .body(request)
        .send()
        .await?;

    let role = response.into_inner();
    println!("Updated role '{}'", role.name);

    if use_json {
        json::print_json(&role)?;
    }

    Ok(())
}

async fn delete_roles(args: RoleDeleteArgs, client: &TypedClient) -> Result<()> {
    for role_ref in &args.roles {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete role '{}'?", role_ref))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        let account = &client.auth_config().account;

        client
            .inner()
            .delete_role()
            .account(account)
            .role(role_ref)
            .send()
            .await?;

        println!("Deleted role '{}'", role_ref);
    }

    Ok(())
}

// =============================================================================
// Policy Implementation
// =============================================================================

async fn list_policies(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_policies()
        .account(account)
        .send()
        .await?;

    let policies = response.into_inner();

    if use_json {
        json::print_json(&policies)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "RULES", "DESCRIPTION"]);
        for policy in &policies {
            tbl.add_row(vec![
                &policy.id.to_string()[..8],
                &policy.name,
                &format!("{} rule(s)", policy.rules.len()),
                policy.description.as_deref().unwrap_or("-"),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_policy(args: PolicyGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .get_policy()
        .account(account)
        .policy(&args.policy)
        .send()
        .await?;

    let policy = response.into_inner();

    if use_json {
        json::print_json(&policy)?;
    } else {
        println!("ID:          {}", policy.id);
        println!("Name:        {}", policy.name);
        println!(
            "Description: {}",
            policy.description.as_deref().unwrap_or("-")
        );
        println!("Rules:");
        for rule in &policy.rules {
            println!("  - {}", rule);
        }
    }

    Ok(())
}

async fn create_policy(args: PolicyCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    if args.rule.is_empty() {
        return Err(anyhow::anyhow!(
            "At least one rule is required. Use --rule to specify rules."
        ));
    }

    let request = cloudapi_client::types::CreatePolicyRequest {
        name: args.name.clone(),
        rules: args.rule,
        description: args.description,
    };

    let response = client
        .inner()
        .create_policy()
        .account(account)
        .body(request)
        .send()
        .await?;

    let policy = response.into_inner();
    println!("Created policy '{}' ({})", policy.name, policy.id);

    if use_json {
        json::print_json(&policy)?;
    }

    Ok(())
}

async fn update_policy(args: PolicyUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::UpdatePolicyRequest {
        name: args.name,
        rules: if args.rule.is_empty() {
            None
        } else {
            Some(args.rule)
        },
        description: args.description,
    };

    let response = client
        .inner()
        .update_policy()
        .account(account)
        .policy(&args.policy)
        .body(request)
        .send()
        .await?;

    let policy = response.into_inner();
    println!("Updated policy '{}'", policy.name);

    if use_json {
        json::print_json(&policy)?;
    }

    Ok(())
}

async fn delete_policies(args: PolicyDeleteArgs, client: &TypedClient) -> Result<()> {
    for policy_ref in &args.policies {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete policy '{}'?", policy_ref))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        let account = &client.auth_config().account;

        client
            .inner()
            .delete_policy()
            .account(account)
            .policy(policy_ref)
            .send()
            .await?;

        println!("Deleted policy '{}'", policy_ref);
    }

    Ok(())
}

// =============================================================================
// RBAC Info Implementation
// =============================================================================

/// RBAC info JSON output structure
#[derive(serde::Serialize)]
struct RbacInfo {
    users: Vec<cloudapi_client::types::User>,
    roles: Vec<cloudapi_client::types::Role>,
    policies: Vec<cloudapi_client::types::Policy>,
}

async fn rbac_info(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // Fetch all RBAC data concurrently
    let (users_result, roles_result, policies_result) = tokio::join!(
        client.inner().list_users().account(account).send(),
        client.inner().list_roles().account(account).send(),
        client.inner().list_policies().account(account).send(),
    );

    let users = users_result?.into_inner();
    let roles = roles_result?.into_inner();
    let policies = policies_result?.into_inner();

    if use_json {
        let info = RbacInfo {
            users,
            roles,
            policies,
        };
        json::print_json(&info)?;
    } else {
        // Summary section
        println!("RBAC Summary");
        println!("============");
        println!("Users:    {}", users.len());
        println!("Roles:    {}", roles.len());
        println!("Policies: {}", policies.len());
        println!();

        // Users section
        if !users.is_empty() {
            println!("Users:");
            let mut tbl = table::create_table(&["SHORTID", "LOGIN", "EMAIL"]);
            for user in &users {
                tbl.add_row(vec![&user.id.to_string()[..8], &user.login, &user.email]);
            }
            table::print_table(tbl);
            println!();
        }

        // Roles section
        if !roles.is_empty() {
            println!("Roles:");
            let mut tbl = table::create_table(&["SHORTID", "NAME", "POLICIES", "MEMBERS"]);
            for role in &roles {
                let policies_str = if role.policies.is_empty() {
                    "-".to_string()
                } else {
                    role.policies.join(", ")
                };
                let members_str = if role.members.is_empty() {
                    "-".to_string()
                } else {
                    role.members.join(", ")
                };
                tbl.add_row(vec![
                    &role.id.to_string()[..8],
                    &role.name,
                    &policies_str,
                    &members_str,
                ]);
            }
            table::print_table(tbl);
            println!();
        }

        // Policies section
        if !policies.is_empty() {
            println!("Policies:");
            let mut tbl = table::create_table(&["SHORTID", "NAME", "RULES"]);
            for policy in &policies {
                tbl.add_row(vec![
                    &policy.id.to_string()[..8],
                    &policy.name,
                    &format!("{} rule(s)", policy.rules.len()),
                ]);
            }
            table::print_table(tbl);
        }
    }

    Ok(())
}

// =============================================================================
// RBAC Apply Implementation
// =============================================================================

/// A change to be applied to RBAC configuration
#[derive(Debug)]
enum RbacChange {
    CreateUser {
        login: String,
        email: String,
        first_name: Option<String>,
        last_name: Option<String>,
        company_name: Option<String>,
    },
    UpdateUser {
        login: String,
        email: Option<String>,
        first_name: Option<String>,
        last_name: Option<String>,
        company_name: Option<String>,
    },
    DeleteUser {
        login: String,
    },
    CreatePolicy {
        name: String,
        description: Option<String>,
        rules: Vec<String>,
    },
    UpdatePolicy {
        name: String,
        description: Option<String>,
        rules: Option<Vec<String>>,
    },
    DeletePolicy {
        name: String,
    },
    CreateRole {
        name: String,
        members: Vec<String>,
        default_members: Vec<String>,
        policies: Vec<String>,
    },
    UpdateRole {
        name: String,
        members: Option<Vec<String>>,
        default_members: Option<Vec<String>>,
        policies: Option<Vec<String>>,
    },
    DeleteRole {
        name: String,
    },
}

impl std::fmt::Display for RbacChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RbacChange::CreateUser { login, .. } => write!(f, "Create user '{}'", login),
            RbacChange::UpdateUser { login, .. } => write!(f, "Update user '{}'", login),
            RbacChange::DeleteUser { login } => write!(f, "Delete user '{}'", login),
            RbacChange::CreatePolicy { name, .. } => write!(f, "Create policy '{}'", name),
            RbacChange::UpdatePolicy { name, .. } => write!(f, "Update policy '{}'", name),
            RbacChange::DeletePolicy { name } => write!(f, "Delete policy '{}'", name),
            RbacChange::CreateRole { name, .. } => write!(f, "Create role '{}'", name),
            RbacChange::UpdateRole { name, .. } => write!(f, "Update role '{}'", name),
            RbacChange::DeleteRole { name } => write!(f, "Delete role '{}'", name),
        }
    }
}

/// Result of applying RBAC configuration
#[derive(serde::Serialize)]
struct ApplyResult {
    changes: Vec<ApplyChangeResult>,
    summary: ApplySummary,
}

#[derive(serde::Serialize)]
struct ApplyChangeResult {
    action: String,
    #[serde(rename = "type")]
    item_type: String,
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct ApplySummary {
    users_created: usize,
    users_updated: usize,
    users_deleted: usize,
    policies_created: usize,
    policies_updated: usize,
    policies_deleted: usize,
    roles_created: usize,
    roles_updated: usize,
    roles_deleted: usize,
}

async fn rbac_apply(args: ApplyArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    // Read and parse the config file
    let content = std::fs::read_to_string(&args.file).map_err(|e| {
        anyhow::anyhow!(
            "Failed to read config file '{}': {}",
            args.file.display(),
            e
        )
    })?;

    let config: RbacConfig = serde_json::from_str(&content).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse config file '{}': {}",
            args.file.display(),
            e
        )
    })?;

    let account = &client.auth_config().account;

    // Fetch current state
    let (users_result, roles_result, policies_result) = tokio::join!(
        client.inner().list_users().account(account).send(),
        client.inner().list_roles().account(account).send(),
        client.inner().list_policies().account(account).send(),
    );

    let current_users = users_result?.into_inner();
    let current_roles = roles_result?.into_inner();
    let current_policies = policies_result?.into_inner();

    // Build maps for quick lookup
    let current_user_map: HashMap<String, _> =
        current_users.iter().map(|u| (u.login.clone(), u)).collect();
    let current_role_map: HashMap<String, _> =
        current_roles.iter().map(|r| (r.name.clone(), r)).collect();
    let current_policy_map: HashMap<String, _> = current_policies
        .iter()
        .map(|p| (p.name.clone(), p))
        .collect();

    // Build desired state sets
    let want_users: HashSet<String> = config.users.iter().map(|u| u.login.clone()).collect();
    let want_roles: HashSet<String> = config.roles.iter().map(|r| r.name.clone()).collect();
    let want_policies: HashSet<String> = config.policies.iter().map(|p| p.name.clone()).collect();

    // Calculate changes
    // Order: create policies first, then users, then roles (roles reference users and policies)
    // Delete order: roles first, then users, then policies
    let mut changes = Vec::new();

    // Policy changes
    for policy in &config.policies {
        if let Some(current) = current_policy_map.get(&policy.name) {
            // Check if update needed
            let rules_differ = {
                let mut current_rules: Vec<_> = current.rules.clone();
                let mut want_rules: Vec<_> = policy.rules.clone();
                current_rules.sort();
                want_rules.sort();
                current_rules != want_rules
            };
            let desc_differs = policy.description != current.description;

            if rules_differ || desc_differs {
                changes.push(RbacChange::UpdatePolicy {
                    name: policy.name.clone(),
                    description: if desc_differs {
                        policy.description.clone()
                    } else {
                        None
                    },
                    rules: if rules_differ {
                        Some(policy.rules.clone())
                    } else {
                        None
                    },
                });
            }
        } else {
            changes.push(RbacChange::CreatePolicy {
                name: policy.name.clone(),
                description: policy.description.clone(),
                rules: policy.rules.clone(),
            });
        }
    }

    // User changes
    for user in &config.users {
        if let Some(current) = current_user_map.get(&user.login) {
            // Check if update needed
            let email_differs = current.email != user.email;
            let fn_differs = current.first_name != user.first_name;
            let ln_differs = current.last_name != user.last_name;
            let cn_differs = current.company_name != user.company_name;

            if email_differs || fn_differs || ln_differs || cn_differs {
                changes.push(RbacChange::UpdateUser {
                    login: user.login.clone(),
                    email: if email_differs {
                        Some(user.email.clone())
                    } else {
                        None
                    },
                    first_name: if fn_differs {
                        user.first_name.clone()
                    } else {
                        None
                    },
                    last_name: if ln_differs {
                        user.last_name.clone()
                    } else {
                        None
                    },
                    company_name: if cn_differs {
                        user.company_name.clone()
                    } else {
                        None
                    },
                });
            }
        } else {
            changes.push(RbacChange::CreateUser {
                login: user.login.clone(),
                email: user.email.clone(),
                first_name: user.first_name.clone(),
                last_name: user.last_name.clone(),
                company_name: user.company_name.clone(),
            });
        }
    }

    // Role changes
    for role in &config.roles {
        if let Some(current) = current_role_map.get(&role.name) {
            // Check if update needed
            let members_differ = {
                let mut cm: Vec<_> = current.members.clone();
                let mut wm: Vec<_> = role.members.clone();
                cm.sort();
                wm.sort();
                cm != wm
            };
            let default_members_differ = {
                let mut cdm: Vec<_> = current.default_members.clone();
                let mut wdm: Vec<_> = role.default_members.clone();
                cdm.sort();
                wdm.sort();
                cdm != wdm
            };
            let policies_differ = {
                let mut cp: Vec<_> = current.policies.clone();
                let mut wp: Vec<_> = role.policies.clone();
                cp.sort();
                wp.sort();
                cp != wp
            };

            if members_differ || default_members_differ || policies_differ {
                changes.push(RbacChange::UpdateRole {
                    name: role.name.clone(),
                    members: if members_differ {
                        Some(role.members.clone())
                    } else {
                        None
                    },
                    default_members: if default_members_differ {
                        Some(role.default_members.clone())
                    } else {
                        None
                    },
                    policies: if policies_differ {
                        Some(role.policies.clone())
                    } else {
                        None
                    },
                });
            }
        } else {
            changes.push(RbacChange::CreateRole {
                name: role.name.clone(),
                members: role.members.clone(),
                default_members: role.default_members.clone(),
                policies: role.policies.clone(),
            });
        }
    }

    // Deletions (roles first, then users, then policies)
    for role in &current_roles {
        if !want_roles.contains(&role.name) {
            changes.push(RbacChange::DeleteRole {
                name: role.name.clone(),
            });
        }
    }
    for user in &current_users {
        if !want_users.contains(&user.login) {
            changes.push(RbacChange::DeleteUser {
                login: user.login.clone(),
            });
        }
    }
    for policy in &current_policies {
        if !want_policies.contains(&policy.name) {
            changes.push(RbacChange::DeletePolicy {
                name: policy.name.clone(),
            });
        }
    }

    // Sort changes: creates first (policies, users, roles), then updates, then deletes (roles, users, policies)
    changes.sort_by_key(|c| match c {
        RbacChange::CreatePolicy { .. } => 0,
        RbacChange::CreateUser { .. } => 1,
        RbacChange::CreateRole { .. } => 2,
        RbacChange::UpdatePolicy { .. } => 3,
        RbacChange::UpdateUser { .. } => 4,
        RbacChange::UpdateRole { .. } => 5,
        RbacChange::DeleteRole { .. } => 6,
        RbacChange::DeleteUser { .. } => 7,
        RbacChange::DeletePolicy { .. } => 8,
    });

    if changes.is_empty() {
        if use_json {
            json::print_json(&ApplyResult {
                changes: vec![],
                summary: ApplySummary {
                    users_created: 0,
                    users_updated: 0,
                    users_deleted: 0,
                    policies_created: 0,
                    policies_updated: 0,
                    policies_deleted: 0,
                    roles_created: 0,
                    roles_updated: 0,
                    roles_deleted: 0,
                },
            })?;
        } else {
            println!("No changes required. RBAC configuration is up to date.");
        }
        return Ok(());
    }

    // Show planned changes
    if !use_json {
        println!("Planned changes:");
        for change in &changes {
            println!("  - {}", change);
        }
        println!();
    }

    // Dry run mode
    if args.dry_run {
        if use_json {
            let change_results: Vec<_> = changes
                .iter()
                .map(|c| {
                    let (action, item_type, name) = match c {
                        RbacChange::CreateUser { login, .. } => ("create", "user", login.clone()),
                        RbacChange::UpdateUser { login, .. } => ("update", "user", login.clone()),
                        RbacChange::DeleteUser { login } => ("delete", "user", login.clone()),
                        RbacChange::CreatePolicy { name, .. } => ("create", "policy", name.clone()),
                        RbacChange::UpdatePolicy { name, .. } => ("update", "policy", name.clone()),
                        RbacChange::DeletePolicy { name } => ("delete", "policy", name.clone()),
                        RbacChange::CreateRole { name, .. } => ("create", "role", name.clone()),
                        RbacChange::UpdateRole { name, .. } => ("update", "role", name.clone()),
                        RbacChange::DeleteRole { name } => ("delete", "role", name.clone()),
                    };
                    ApplyChangeResult {
                        action: action.to_string(),
                        item_type: item_type.to_string(),
                        name,
                        status: "dry-run".to_string(),
                        error: None,
                    }
                })
                .collect();
            json::print_json(&serde_json::json!({
                "dry_run": true,
                "changes": change_results,
            }))?;
        } else {
            println!("[dry-run] {} change(s) would be applied.", changes.len());
        }
        return Ok(());
    }

    // Confirm if not forced
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt(format!("Apply {} change(s)?", changes.len()))
            .default(false)
            .interact()?
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Execute changes
    let mut summary = ApplySummary {
        users_created: 0,
        users_updated: 0,
        users_deleted: 0,
        policies_created: 0,
        policies_updated: 0,
        policies_deleted: 0,
        roles_created: 0,
        roles_updated: 0,
        roles_deleted: 0,
    };
    let mut results = Vec::new();

    for change in &changes {
        let result = execute_rbac_change(change, client).await;

        let (action, item_type, name) = match change {
            RbacChange::CreateUser { login, .. } => ("create", "user", login.clone()),
            RbacChange::UpdateUser { login, .. } => ("update", "user", login.clone()),
            RbacChange::DeleteUser { login } => ("delete", "user", login.clone()),
            RbacChange::CreatePolicy { name, .. } => ("create", "policy", name.clone()),
            RbacChange::UpdatePolicy { name, .. } => ("update", "policy", name.clone()),
            RbacChange::DeletePolicy { name } => ("delete", "policy", name.clone()),
            RbacChange::CreateRole { name, .. } => ("create", "role", name.clone()),
            RbacChange::UpdateRole { name, .. } => ("update", "role", name.clone()),
            RbacChange::DeleteRole { name } => ("delete", "role", name.clone()),
        };

        match &result {
            Ok(()) => {
                if !use_json {
                    println!("  {} {}", action, name);
                }
                match change {
                    RbacChange::CreateUser { .. } => summary.users_created += 1,
                    RbacChange::UpdateUser { .. } => summary.users_updated += 1,
                    RbacChange::DeleteUser { .. } => summary.users_deleted += 1,
                    RbacChange::CreatePolicy { .. } => summary.policies_created += 1,
                    RbacChange::UpdatePolicy { .. } => summary.policies_updated += 1,
                    RbacChange::DeletePolicy { .. } => summary.policies_deleted += 1,
                    RbacChange::CreateRole { .. } => summary.roles_created += 1,
                    RbacChange::UpdateRole { .. } => summary.roles_updated += 1,
                    RbacChange::DeleteRole { .. } => summary.roles_deleted += 1,
                }
                results.push(ApplyChangeResult {
                    action: action.to_string(),
                    item_type: item_type.to_string(),
                    name,
                    status: "success".to_string(),
                    error: None,
                });
            }
            Err(e) => {
                if !use_json {
                    println!("  {} {} - FAILED: {}", action, name, e);
                }
                results.push(ApplyChangeResult {
                    action: action.to_string(),
                    item_type: item_type.to_string(),
                    name,
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                });
            }
        }
    }

    if use_json {
        json::print_json(&ApplyResult {
            changes: results,
            summary,
        })?;
    } else {
        println!();
        println!("Summary:");
        if summary.users_created > 0 || summary.users_updated > 0 || summary.users_deleted > 0 {
            println!(
                "  Users: {} created, {} updated, {} deleted",
                summary.users_created, summary.users_updated, summary.users_deleted
            );
        }
        if summary.policies_created > 0
            || summary.policies_updated > 0
            || summary.policies_deleted > 0
        {
            println!(
                "  Policies: {} created, {} updated, {} deleted",
                summary.policies_created, summary.policies_updated, summary.policies_deleted
            );
        }
        if summary.roles_created > 0 || summary.roles_updated > 0 || summary.roles_deleted > 0 {
            println!(
                "  Roles: {} created, {} updated, {} deleted",
                summary.roles_created, summary.roles_updated, summary.roles_deleted
            );
        }
    }

    Ok(())
}

/// Generate a random password for new users
fn generate_password() -> String {
    use std::fmt::Write;
    let mut rng = [0u8; 24];
    // Use a simple random source
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // Mix in some entropy from time
    let seed = now.as_nanos();
    for (i, b) in rng.iter_mut().enumerate() {
        *b = ((seed >> (i * 3)) & 0xFF) as u8 ^ (i as u8 * 17);
    }
    let mut result = String::with_capacity(32);
    for b in &rng {
        write!(result, "{:02x}", b).unwrap();
    }
    result
}

async fn execute_rbac_change(change: &RbacChange, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    match change {
        RbacChange::CreateUser {
            login,
            email,
            first_name,
            last_name,
            company_name,
        } => {
            let request = cloudapi_client::types::CreateUserRequest {
                login: login.clone(),
                email: email.clone(),
                password: generate_password(),
                company_name: company_name.clone(),
                first_name: first_name.clone(),
                last_name: last_name.clone(),
                phone: None,
            };
            client
                .inner()
                .create_user()
                .account(account)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::UpdateUser {
            login,
            email,
            first_name,
            last_name,
            company_name,
        } => {
            let user_id = resolve_user(login, client).await?;
            let request = cloudapi_client::types::UpdateUserRequest {
                email: email.clone(),
                company_name: company_name.clone(),
                first_name: first_name.clone(),
                last_name: last_name.clone(),
                phone: None,
            };
            client
                .inner()
                .update_user()
                .account(account)
                .uuid(&user_id)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::DeleteUser { login } => {
            let user_id = resolve_user(login, client).await?;
            client
                .inner()
                .delete_user()
                .account(account)
                .uuid(&user_id)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::CreatePolicy {
            name,
            description,
            rules,
        } => {
            let request = cloudapi_client::types::CreatePolicyRequest {
                name: name.clone(),
                rules: rules.clone(),
                description: description.clone(),
            };
            client
                .inner()
                .create_policy()
                .account(account)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::UpdatePolicy {
            name,
            description,
            rules,
        } => {
            let request = cloudapi_client::types::UpdatePolicyRequest {
                name: None,
                rules: rules.clone(),
                description: description.clone(),
            };
            client
                .inner()
                .update_policy()
                .account(account)
                .policy(name)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::DeletePolicy { name } => {
            client
                .inner()
                .delete_policy()
                .account(account)
                .policy(name)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::CreateRole {
            name,
            members,
            default_members,
            policies,
        } => {
            let request = cloudapi_client::types::CreateRoleRequest {
                name: name.clone(),
                policies: if policies.is_empty() {
                    None
                } else {
                    Some(policies.clone())
                },
                members: if members.is_empty() {
                    None
                } else {
                    Some(members.clone())
                },
                default_members: if default_members.is_empty() {
                    None
                } else {
                    Some(default_members.clone())
                },
            };
            client
                .inner()
                .create_role()
                .account(account)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::UpdateRole {
            name,
            members,
            default_members,
            policies,
        } => {
            let request = cloudapi_client::types::UpdateRoleRequest {
                name: None,
                policies: policies.clone(),
                members: members.clone(),
                default_members: default_members.clone(),
            };
            client
                .inner()
                .update_role()
                .account(account)
                .role(name)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::DeleteRole { name } => {
            client
                .inner()
                .delete_role()
                .account(account)
                .role(name)
                .send()
                .await?;
            Ok(())
        }
    }
}

// =============================================================================
// RBAC Reset Implementation
// =============================================================================

async fn rbac_reset(args: ResetArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    // Fetch current state
    let (users_result, roles_result, policies_result) = tokio::join!(
        client.inner().list_users().account(account).send(),
        client.inner().list_roles().account(account).send(),
        client.inner().list_policies().account(account).send(),
    );

    let users = users_result?.into_inner();
    let roles = roles_result?.into_inner();
    let policies = policies_result?.into_inner();

    let total = users.len() + roles.len() + policies.len();

    if total == 0 {
        println!("No RBAC configuration to reset.");
        return Ok(());
    }

    println!("This will delete:");
    if !users.is_empty() {
        println!(
            "  - {} user(s): {}",
            users.len(),
            users
                .iter()
                .map(|u| u.login.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !roles.is_empty() {
        println!(
            "  - {} role(s): {}",
            roles.len(),
            roles
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !policies.is_empty() {
        println!(
            "  - {} policy(ies): {}",
            policies.len(),
            policies
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!();

    // Confirm if not forced
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt("Are you sure you want to delete all RBAC configuration?")
            .default(false)
            .interact()?
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Delete in order: roles first (they reference users/policies), then users, then policies
    let mut errors = Vec::new();

    // Delete roles
    for role in &roles {
        if let Err(e) = client
            .inner()
            .delete_role()
            .account(account)
            .role(&role.name)
            .send()
            .await
        {
            errors.push(format!("Failed to delete role '{}': {}", role.name, e));
        } else {
            println!("Deleted role '{}'", role.name);
        }
    }

    // Delete users
    for user in &users {
        if let Err(e) = client
            .inner()
            .delete_user()
            .account(account)
            .uuid(user.id.to_string())
            .send()
            .await
        {
            errors.push(format!("Failed to delete user '{}': {}", user.login, e));
        } else {
            println!("Deleted user '{}'", user.login);
        }
    }

    // Delete policies
    for policy in &policies {
        if let Err(e) = client
            .inner()
            .delete_policy()
            .account(account)
            .policy(&policy.name)
            .send()
            .await
        {
            errors.push(format!("Failed to delete policy '{}': {}", policy.name, e));
        } else {
            println!("Deleted policy '{}'", policy.name);
        }
    }

    if !errors.is_empty() {
        println!();
        println!("Errors occurred:");
        for err in &errors {
            println!("  - {}", err);
        }
        return Err(anyhow::anyhow!(
            "{} error(s) occurred during reset",
            errors.len()
        ));
    }

    println!();
    println!("RBAC configuration reset complete.");

    Ok(())
}
