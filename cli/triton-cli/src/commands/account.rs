// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Account management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use std::path::PathBuf;

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
    /// Field updates in KEY=VALUE format (e.g., email=new@example.com)
    #[arg(value_name = "FIELD=VALUE")]
    pub fields: Vec<String>,
    /// Update account from JSON file
    #[arg(short = 'f', long = "file", conflicts_with_all = ["email", "given_name", "surname", "company_name", "phone", "fields"])]
    pub file: Option<PathBuf>,
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

/// Format a duration as a human-readable relative time string (e.g., "1d", "41w")
fn long_ago(when: &str) -> String {
    use chrono::{DateTime, Utc};

    let parsed = match DateTime::parse_from_rfc3339(when) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return "".to_string(),
    };

    let now = Utc::now();
    let duration = now.signed_duration_since(parsed);
    let seconds = duration.num_seconds();

    if seconds < 0 {
        return "0s".to_string();
    }

    let years = seconds / (60 * 60 * 24 * 365);
    if years > 0 {
        return format!("{}y", years);
    }

    let weeks = seconds / (60 * 60 * 24 * 7);
    if weeks > 0 {
        return format!("{}w", weeks);
    }

    let days = seconds / (60 * 60 * 24);
    if days > 0 {
        return format!("{}d", days);
    }

    let hours = seconds / (60 * 60);
    if hours > 0 {
        return format!("{}h", hours);
    }

    let minutes = seconds / 60;
    if minutes > 0 {
        return format!("{}m", minutes);
    }

    format!("{}s", seconds)
}

async fn get_account(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client.inner().get_account().account(account).send().await?;

    let acc = response.into_inner();

    if use_json {
        json::print_json(&acc)?;
    } else {
        // Match node-triton output format: key: value, with relative time for dates
        println!("id: {}", acc.id);
        println!("login: {}", acc.login);
        println!("email: {}", acc.email);
        if let Some(company) = &acc.company_name {
            println!("companyName: {}", company);
        }
        if let Some(first) = &acc.first_name {
            println!("firstName: {}", first);
        }
        if let Some(last) = &acc.last_name {
            println!("lastName: {}", last);
        }
        if let Some(cns) = acc.triton_cns_enabled {
            println!("triton_cns_enabled: {}", cns);
        }
        if let Some(phone) = &acc.phone {
            println!("phone: {}", phone);
        }
        println!("updated: {} ({})", acc.updated, long_ago(&acc.updated));
        println!("created: {} ({})", acc.created, long_ago(&acc.created));
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
        // Convert the object-style response to array format for node-triton compatibility
        let json_value = serde_json::to_value(&limits)?;
        let limits_array: Vec<serde_json::Value> =
            if let serde_json::Value::Object(map) = json_value {
                map.into_iter()
                    .filter_map(|(key, value)| {
                        // Only include non-null values
                        if value.is_null() {
                            None
                        } else {
                            Some(serde_json::json!({
                                "type": key,
                                "limit": value,
                                "used": 0  // API doesn't provide used values in this format
                            }))
                        }
                    })
                    .collect()
            } else {
                vec![]
            };
        json::print_json(&limits_array)?;
    } else {
        // Match node-triton output: table with TYPE, USED, LIMIT columns
        println!("{:<10} {:>5}  {:>5}", "TYPE", "USED", "LIMIT");

        let json_value = serde_json::to_value(&limits)?;
        if let serde_json::Value::Object(map) = json_value {
            for (key, value) in map {
                if !value.is_null() {
                    println!("{:<10} {:>5}  {:>5}", key, "-", value);
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

    // Handle file-based input
    if let Some(file_path) = &args.file {
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| anyhow::anyhow!("Failed to read file '{}': {}", file_path.display(), e))?;

        let request: cloudapi_client::types::UpdateAccountRequest = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse JSON: {}", e))?;

        let response = client
            .inner()
            .update_account()
            .account(account)
            .body(request)
            .send()
            .await?;
        let acc = response.into_inner();

        println!("Account updated from file");

        if use_json {
            json::print_json(&acc)?;
        }

        return Ok(());
    }

    // Start with CLI flag values
    let mut email = args.email.clone();
    let mut given_name = args.given_name.clone();
    let mut surname = args.surname.clone();
    let mut company_name = args.company_name.clone();
    let mut phone = args.phone.clone();

    // Parse FIELD=VALUE arguments
    for field in &args.fields {
        if let Some((key, value)) = field.split_once('=') {
            match key {
                "email" => email = Some(value.to_string()),
                "givenName" | "given_name" | "firstName" | "first_name" => {
                    given_name = Some(value.to_string())
                }
                "sn" | "surname" | "lastName" | "last_name" => surname = Some(value.to_string()),
                "company" | "companyName" | "company_name" => {
                    company_name = Some(value.to_string())
                }
                "phone" => phone = Some(value.to_string()),
                _ => return Err(anyhow::anyhow!("Unknown field: {}", key)),
            }
        } else {
            return Err(anyhow::anyhow!(
                "Invalid field format '{}', expected KEY=VALUE",
                field
            ));
        }
    }

    let request = cloudapi_client::types::UpdateAccountRequest {
        email,
        first_name: given_name,
        last_name: surname,
        company_name,
        phone,
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
