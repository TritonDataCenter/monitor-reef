// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC role management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{json, table};

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
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
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

pub async fn list_roles(client: &TypedClient, use_json: bool) -> Result<()> {
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

pub async fn delete_roles(args: RoleDeleteArgs, client: &TypedClient) -> Result<()> {
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
