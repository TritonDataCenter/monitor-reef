// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance firewall commands

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::FirewallRule;

use crate::define_columns;
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct EnableFirewallArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,
}

#[derive(Args, Clone)]
pub struct DisableFirewallArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,
}

#[derive(Args, Clone)]
pub struct FwrulesArgs {
    /// Instance ID or name
    pub instance: String,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

pub async fn enable(args: EnableFirewallArgs, client: &TypedClient) -> Result<()> {
    let account = client.effective_account();

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let id_str = machine_id.to_string();

        client.enable_firewall(account, &machine_id, None).await?;

        println!("Enabled firewall for instance {}", &id_str[..8]);
    }

    Ok(())
}

pub async fn disable(args: DisableFirewallArgs, client: &TypedClient) -> Result<()> {
    let account = client.effective_account();

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let id_str = machine_id.to_string();

        client.disable_firewall(account, &machine_id, None).await?;

        println!("Disabled firewall for instance {}", &id_str[..8]);
    }

    Ok(())
}

pub async fn list_rules(args: FwrulesArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let response = client
        .inner()
        .list_machine_firewall_rules()
        .account(account)
        .machine(machine_id)
        .send()
        .await?;

    let mut rules = response.into_inner();
    rules.sort_by(|a, b| a.rule.cmp(&b.rule));

    if use_json {
        json::print_json_stream(&rules)?;
    } else {
        define_columns! {
            FwColumn for FirewallRule, long_from: 5, {
                ShortId("SHORTID") => |rule| rule.id.to_string()[..8].to_string(),
                Enabled("ENABLED") => |rule| {
                    if rule.enabled { "yes" } else { "no" }.to_string()
                },
                Global("GLOBAL") => |rule| {
                    rule.global.map(|g| if g { "yes" } else { "no" })
                        .unwrap_or("-").to_string()
                },
                Log("LOG") => |rule| {
                    if rule.log { "yes" } else { "no" }.to_string()
                },
                Rule("RULE") => |rule| rule.rule.clone(),
                // --- long-only columns below ---
                Id("ID") => |rule| rule.id.to_string(),
                Description("DESCRIPTION") => |rule| {
                    rule.description.clone().unwrap_or_else(|| "-".to_string())
                },
            }
        }

        TableBuilder::from_enum_columns::<FwColumn, _>(&rules, Some(FwColumn::LONG_FROM))
            .print(&args.table)?;
    }

    Ok(())
}
