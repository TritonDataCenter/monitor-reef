// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Account management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json;

#[derive(Subcommand, Clone)]
pub enum AccountCommand {
    /// Get account details
    Get,
    /// Get account resource limits
    Limits,
    /// Update account settings
    Update(AccountUpdateArgs),
}

#[derive(Args, Clone)]
pub struct AccountUpdateArgs {
    /// New email
    #[arg(long)]
    pub email: Option<String>,
    /// Given name
    #[arg(long)]
    pub given_name: Option<String>,
    /// Surname
    #[arg(long)]
    pub surname: Option<String>,
    /// Company name
    #[arg(long)]
    pub company_name: Option<String>,
    /// Phone number
    #[arg(long)]
    pub phone: Option<String>,
}

impl AccountCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Get => get_account(client, use_json).await,
            Self::Limits => get_limits(client, use_json).await,
            Self::Update(args) => update_account(args, client, use_json).await,
        }
    }
}

async fn get_account(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client.inner().get_account().account(account).send().await?;

    let acc = response.into_inner();

    if use_json {
        json::print_json(&acc)?;
    } else {
        println!("Login:     {}", acc.login);
        println!("Email:     {}", acc.email);
        if let Some(name) = &acc.first_name {
            println!(
                "Name:      {} {}",
                name,
                acc.last_name.as_deref().unwrap_or("")
            );
        }
        if let Some(company) = &acc.company_name {
            println!("Company:   {}", company);
        }
        if let Some(phone) = &acc.phone {
            println!("Phone:     {}", phone);
        }
        println!("Created:   {}", acc.created);
        println!("Updated:   {}", acc.updated);
    }

    Ok(())
}

async fn get_limits(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .get_provisioning_limits()
        .account(account)
        .send()
        .await?;

    let limits = response.into_inner();

    if use_json {
        json::print_json(&limits)?;
    } else {
        println!("Provisioning Limits:");
        // The ProvisioningLimits struct contains datacenter-specific limits
        // Display them in a readable format
        let json_value = serde_json::to_value(&limits)?;
        if let serde_json::Value::Object(map) = json_value {
            for (dc, dc_limits) in map {
                println!("\n  {}:", dc);
                if let serde_json::Value::Object(limits_map) = dc_limits {
                    for (key, value) in limits_map {
                        println!("    {}: {}", key, value);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn update_account(
    args: AccountUpdateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::UpdateAccountRequest {
        email: args.email.clone(),
        first_name: args.given_name.clone(),
        last_name: args.surname.clone(),
        company_name: args.company_name.clone(),
        phone: args.phone.clone(),
        address: None,
        postal_code: None,
        city: None,
        state: None,
        country: None,
        triton_cns_enabled: None,
    };

    let response = client
        .inner()
        .update_account()
        .account(account)
        .body(request)
        .send()
        .await?;
    let acc = response.into_inner();

    println!("Account updated");

    if use_json {
        json::print_json(&acc)?;
    }

    Ok(())
}
