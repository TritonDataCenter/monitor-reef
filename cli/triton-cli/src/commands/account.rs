// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Account management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_api::Account;
use std::path::PathBuf;

use crate::client::AnyClient;
use crate::dispatch;
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
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
        match self {
            Self::Get => get_account(client, use_json).await,
            Self::Limits => get_limits(client, use_json).await,
            Self::Update(args) => update_account(args, client, use_json).await,
        }
    }
}

/// Display an Option as its value or "null", matching node-triton output.
fn opt_display<T: std::fmt::Display>(opt: &Option<T>) -> String {
    match opt {
        Some(v) => v.to_string(),
        None => "null".to_string(),
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

async fn get_account(client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();

    let acc: Account = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_account()
            .account(account)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Account>(serde_json::to_value(&resp)?)?
    });

    if use_json {
        json::print_json(&acc)?;
    } else {
        // Match node-triton output format: key: value, with relative time for dates
        println!("id: {}", acc.id);
        println!("login: {}", acc.login);
        println!("email: {}", acc.email);
        println!("companyName: {}", opt_display(&acc.company_name));
        println!("firstName: {}", opt_display(&acc.first_name));
        println!("lastName: {}", opt_display(&acc.last_name));
        println!("postalCode: {}", opt_display(&acc.postal_code));
        println!(
            "triton_cns_enabled: {}",
            opt_display(&acc.triton_cns_enabled)
        );
        println!("address: {}", opt_display(&acc.address));
        println!("city: {}", opt_display(&acc.city));
        println!("state: {}", opt_display(&acc.state));
        println!("country: {}", opt_display(&acc.country));
        println!("phone: {}", opt_display(&acc.phone));
        println!("updated: {} ({})", acc.updated, long_ago(&acc.updated));
        println!("created: {} ({})", acc.created, long_ago(&acc.created));
    }

    Ok(())
}

async fn get_limits(client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();

    let limits_json: serde_json::Value = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_provisioning_limits()
            .account(account)
            .send()
            .await?
            .into_inner();
        serde_json::to_value(&resp)?
    });

    if use_json {
        // Convert the object-style response to array format for node-triton compatibility
        let limits_array: Vec<serde_json::Value> =
            if let serde_json::Value::Object(map) = &limits_json {
                map.iter()
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

        if let serde_json::Value::Object(map) = &limits_json {
            for (key, value) in map {
                if !value.is_null() {
                    println!("{:<10} {:>5}  {:>5}", key, "-", value);
                }
            }
        }
    }

    Ok(())
}

async fn update_account(args: AccountUpdateArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();

    // Build the update body as a plain JSON Value so each dispatch arm
    // can deserialize into its own per-client
    // `UpdateAccountRequest` struct.
    let body_value: serde_json::Value = if let Some(file_path) = &args.file {
        let content = tokio::fs::read_to_string(file_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read file '{}': {}", file_path.display(), e))?;
        serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse JSON: {}", e))?
    } else {
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
                    "sn" | "surname" | "lastName" | "last_name" => {
                        surname = Some(value.to_string())
                    }
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

        let mut obj = serde_json::Map::new();
        if let Some(v) = email {
            obj.insert("email".into(), serde_json::Value::String(v));
        }
        if let Some(v) = given_name {
            obj.insert("firstName".into(), serde_json::Value::String(v));
        }
        if let Some(v) = surname {
            obj.insert("lastName".into(), serde_json::Value::String(v));
        }
        if let Some(v) = company_name {
            obj.insert("companyName".into(), serde_json::Value::String(v));
        }
        if let Some(v) = phone {
            obj.insert("phone".into(), serde_json::Value::String(v));
        }
        serde_json::Value::Object(obj)
    };

    let acc: Account = crate::dispatch_with_types!(client, |c, t| {
        let request: t::UpdateAccountRequest = serde_json::from_value(body_value.clone())?;
        let resp = c
            .inner()
            .update_account()
            .account(account)
            .body(request)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Account>(serde_json::to_value(&resp)?)?
    });

    eprintln!("Account updated");

    if use_json {
        json::print_json(&acc)?;
    }

    Ok(())
}
