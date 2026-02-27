// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance firewall commands

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use cloudapi_client::types::FirewallRule;

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

    let rules = response.into_inner();

    if use_json {
        json::print_json_stream(&rules)?;
    } else {
        let short_cols = ["shortid", "enabled", "global", "log", "rule"];
        let long_cols = ["id", "description"];

        let mut tbl = TableBuilder::new(&["SHORTID", "ENABLED", "GLOBAL", "LOG", "RULE"])
            .with_long_headers(&["ID", "DESCRIPTION"]);

        let all_cols: Vec<&str> = short_cols.iter().chain(long_cols.iter()).copied().collect();
        for rule in &rules {
            let row = all_cols
                .iter()
                .map(|col| get_instance_fwrule_field_value(rule, col))
                .collect();
            tbl.add_row(row);
        }

        tbl.print(&args.table);
    }

    Ok(())
}

fn get_instance_fwrule_field_value(rule: &FirewallRule, field: &str) -> String {
    match field.to_lowercase().as_str() {
        "id" => rule.id.to_string(),
        "shortid" => rule.id.to_string()[..8].to_string(),
        "enabled" => if rule.enabled { "yes" } else { "no" }.to_string(),
        "global" => rule
            .global
            .map(|g| if g { "yes" } else { "no" })
            .unwrap_or("-")
            .to_string(),
        "log" => if rule.log { "yes" } else { "no" }.to_string(),
        "rule" => rule.rule.clone(),
        "description" | "desc" => rule.description.clone().unwrap_or_else(|| "-".to_string()),
        _ => "-".to_string(),
    }
}
