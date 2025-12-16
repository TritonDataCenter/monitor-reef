// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC (Role-Based Access Control) management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum RbacCommand {
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
            Self::User { command } => command.run(client, use_json).await,
            Self::Role { command } => command.run(client, use_json).await,
            Self::Policy { command } => command.run(client, use_json).await,
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
