// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC policy management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{json, table};

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
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
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

pub async fn list_policies(client: &TypedClient, use_json: bool) -> Result<()> {
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

pub async fn delete_policies(args: PolicyDeleteArgs, client: &TypedClient) -> Result<()> {
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
