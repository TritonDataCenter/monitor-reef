// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance firewall commands

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

use crate::output::{json, table};

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
}

pub async fn enable(args: EnableFirewallArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;

        client
            .enable_firewall(account, &machine_id.parse()?, None)
            .await?;

        println!("Enabled firewall for instance {}", &machine_id[..8]);
    }

    Ok(())
}

pub async fn disable(args: DisableFirewallArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;

        client
            .disable_firewall(account, &machine_id.parse()?, None)
            .await?;

        println!("Disabled firewall for instance {}", &machine_id[..8]);
    }

    Ok(())
}

pub async fn list_rules(args: FwrulesArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .list_machine_firewall_rules()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let rules = response.into_inner();

    if use_json {
        json::print_json_stream(&rules)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "ENABLED", "RULE"]);

        for rule in &rules {
            let short_id = &rule.id.to_string()[..8];
            let enabled = if rule.enabled { "yes" } else { "no" };

            tbl.add_row(vec![short_id, enabled, &rule.rule]);
        }

        table::print_table(tbl);
    }

    Ok(())
}
