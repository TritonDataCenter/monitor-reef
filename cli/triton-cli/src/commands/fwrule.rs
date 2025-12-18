// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Firewall rule management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct FwruleListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Subcommand, Clone)]
pub enum FwruleCommand {
    /// List firewall rules
    #[command(visible_alias = "ls")]
    List(FwruleListArgs),
    /// Get firewall rule details
    Get(FwruleGetArgs),
    /// Create firewall rule
    Create(FwruleCreateArgs),
    /// Delete firewall rule(s)
    #[command(visible_alias = "rm")]
    Delete(FwruleDeleteArgs),
    /// Enable firewall rule(s)
    Enable(FwruleEnableArgs),
    /// Disable firewall rule(s)
    Disable(FwruleDisableArgs),
    /// Update firewall rule
    Update(FwruleUpdateArgs),
    /// List instances affected by rule
    #[command(visible_alias = "insts")]
    Instances(FwruleInstancesArgs),
}

#[derive(Args, Clone)]
pub struct FwruleGetArgs {
    /// Rule ID
    pub id: String,
}

#[derive(Args, Clone)]
pub struct FwruleCreateArgs {
    /// Rule text (e.g., "FROM any TO vm <uuid> ALLOW tcp PORT 22")
    pub rule: String,
    /// Rule description
    #[arg(short = 'D', long)]
    pub description: Option<String>,
    /// Create rule in disabled state (rules are enabled by default)
    #[arg(short = 'd', long)]
    pub disabled: bool,
    /// Enable TCP connection logging for this rule
    #[arg(short = 'l', long)]
    pub log: bool,
}

#[derive(Args, Clone)]
pub struct FwruleDeleteArgs {
    /// Rule ID(s)
    pub ids: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct FwruleEnableArgs {
    /// Rule ID(s)
    pub ids: Vec<String>,
}

#[derive(Args, Clone)]
pub struct FwruleDisableArgs {
    /// Rule ID(s)
    pub ids: Vec<String>,
}

#[derive(Args, Clone)]
pub struct FwruleUpdateArgs {
    /// Rule ID
    pub id: String,
    /// New rule text
    #[arg(long)]
    pub rule: Option<String>,
    /// New description
    #[arg(long)]
    pub description: Option<String>,
    /// Enable TCP connection logging for this rule
    #[arg(long = "log")]
    pub enable_log: Option<bool>,
}

#[derive(Args, Clone)]
pub struct FwruleInstancesArgs {
    /// Rule ID
    pub id: String,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

impl FwruleCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_rules(args, client, use_json).await,
            Self::Get(args) => get_rule(args, client, use_json).await,
            Self::Create(args) => create_rule(args, client, use_json).await,
            Self::Delete(args) => delete_rules(args, client).await,
            Self::Enable(args) => enable_rules(args, client).await,
            Self::Disable(args) => disable_rules(args, client).await,
            Self::Update(args) => update_rule(args, client, use_json).await,
            Self::Instances(args) => list_rule_instances(args, client, use_json).await,
        }
    }
}

async fn list_rules(args: FwruleListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_firewall_rules()
        .account(account)
        .send()
        .await?;

    let rules = response.into_inner();

    if use_json {
        json::print_json_stream(&rules)?;
    } else {
        // node-triton columns: SHORTID, ENABLED, GLOBAL, LOG, RULE
        let mut tbl = TableBuilder::new(&["SHORTID", "ENABLED", "GLOBAL", "LOG", "RULE"])
            .with_long_headers(&["ID", "DESCRIPTION"]);
        for rule in &rules {
            tbl.add_row(vec![
                rule.id.to_string()[..8].to_string(),
                if rule.enabled { "true" } else { "false" }.to_string(),
                if rule.global.unwrap_or(false) {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
                if rule.log { "true" } else { "false" }.to_string(),
                rule.rule.clone(),
                rule.id.to_string(),
                rule.description.clone().unwrap_or_else(|| "-".to_string()),
            ]);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

async fn get_rule(args: FwruleGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let rule_id = resolve_rule(&args.id, client).await?;

    let response = client
        .inner()
        .get_firewall_rule()
        .account(account)
        .id(&rule_id)
        .send()
        .await?;

    let rule = response.into_inner();

    if use_json {
        json::print_json(&rule)?;
    } else {
        println!("ID:          {}", rule.id);
        println!("Rule:        {}", rule.rule);
        println!("Enabled:     {}", rule.enabled);
        println!("Log:         {}", rule.log);
        if let Some(desc) = &rule.description {
            println!("Description: {}", desc);
        }
    }

    Ok(())
}

async fn create_rule(args: FwruleCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::CreateFirewallRuleRequest {
        rule: args.rule.clone(),
        enabled: Some(!args.disabled),
        log: if args.log { Some(true) } else { None },
        description: args.description.clone(),
    };

    let response = client
        .inner()
        .create_firewall_rule()
        .account(account)
        .body(request)
        .send()
        .await?;
    let rule = response.into_inner();

    println!(
        "Created firewall rule {} ({}{})",
        &rule.id.to_string()[..8],
        if rule.enabled { "enabled" } else { "disabled" },
        if rule.log { ", logging" } else { "" }
    );

    if use_json {
        json::print_json(&rule)?;
    }

    Ok(())
}

async fn delete_rules(args: FwruleDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for rule_id in &args.ids {
        let resolved_id = resolve_rule(rule_id, client).await?;

        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete firewall rule '{}'?", rule_id))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        client
            .inner()
            .delete_firewall_rule()
            .account(account)
            .id(&resolved_id)
            .send()
            .await?;

        println!("Deleted firewall rule {}", rule_id);
    }

    Ok(())
}

async fn enable_rules(args: FwruleEnableArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for rule_id in &args.ids {
        let resolved_id = resolve_rule(rule_id, client).await?;

        let request = cloudapi_client::types::UpdateFirewallRuleRequest {
            rule: None,
            enabled: Some(true),
            log: None,
            description: None,
        };

        client
            .inner()
            .update_firewall_rule()
            .account(account)
            .id(&resolved_id)
            .body(request)
            .send()
            .await?;

        println!("Enabled firewall rule {}", rule_id);
    }

    Ok(())
}

async fn disable_rules(args: FwruleDisableArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for rule_id in &args.ids {
        let resolved_id = resolve_rule(rule_id, client).await?;

        let request = cloudapi_client::types::UpdateFirewallRuleRequest {
            rule: None,
            enabled: Some(false),
            log: None,
            description: None,
        };

        client
            .inner()
            .update_firewall_rule()
            .account(account)
            .id(&resolved_id)
            .body(request)
            .send()
            .await?;

        println!("Disabled firewall rule {}", rule_id);
    }

    Ok(())
}

async fn update_rule(args: FwruleUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let rule_id = resolve_rule(&args.id, client).await?;

    let request = cloudapi_client::types::UpdateFirewallRuleRequest {
        rule: args.rule.clone(),
        enabled: None,
        log: args.enable_log,
        description: args.description.clone(),
    };

    let response = client
        .inner()
        .update_firewall_rule()
        .account(account)
        .id(&rule_id)
        .body(request)
        .send()
        .await?;
    let rule = response.into_inner();

    println!("Updated firewall rule {}", &rule.id.to_string()[..8]);

    if use_json {
        json::print_json(&rule)?;
    }

    Ok(())
}

async fn list_rule_instances(
    args: FwruleInstancesArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let rule_id = resolve_rule(&args.id, client).await?;

    let response = client
        .inner()
        .list_firewall_rule_machines()
        .account(account)
        .id(&rule_id)
        .send()
        .await?;

    let machines = response.into_inner();

    if use_json {
        json::print_json(&machines)?;
    } else {
        let mut tbl = TableBuilder::new(&["SHORTID", "NAME", "STATE", "PRIMARY_IP"])
            .with_long_headers(&["ID", "IMAGE", "MEMORY"]);
        for m in &machines {
            tbl.add_row(vec![
                m.id.to_string()[..8].to_string(),
                m.name.clone(),
                format!("{:?}", m.state).to_lowercase(),
                m.primary_ip.clone().unwrap_or_else(|| "-".to_string()),
                m.id.to_string(),
                m.image.to_string(),
                m.memory.to_string(),
            ]);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

/// Resolve firewall rule short ID to full UUID
async fn resolve_rule(id_or_short: &str, client: &TypedClient) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_short).is_ok() {
        return Ok(id_or_short.to_string());
    }

    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_firewall_rules()
        .account(account)
        .send()
        .await?;

    let rules = response.into_inner();

    // Try short ID match (at least 8 characters)
    if id_or_short.len() >= 8 {
        for rule in &rules {
            if rule.id.to_string().starts_with(id_or_short) {
                return Ok(rule.id.to_string());
            }
        }
    }

    Err(anyhow::anyhow!("Firewall rule not found: {}", id_or_short))
}
