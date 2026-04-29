// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Account management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;
use triton_gateway_client::TypedClient;

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

/// Display an Option as its value or "null", matching node-triton output.
fn opt_display<T: std::fmt::Display>(opt: &Option<T>) -> String {
    match opt {
        Some(v) => v.to_string(),
        None => "null".to_string(),
    }
}

/// Format a duration as a human-readable relative time string (e.g., "1d", "41w")
fn long_ago(when: &chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Utc;

    let now = Utc::now();
    let duration = now.signed_duration_since(*when);
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
    let account = client.effective_account();
    let response = client.inner().get_account().account(account).send().await?;

    let acc = response.into_inner();

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

async fn get_limits(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
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
        // Match node-triton output: table with BY, LIMIT, USED, CHECK columns
        println!("{:<10} {:>7}  {:>5}  CHECK", "BY", "LIMIT", "USED");

        for limit in &limits {
            let by = limit.by.as_deref().unwrap_or("machines");
            let used = limit
                .used
                .map(|u| u.to_string())
                .unwrap_or_else(|| "-".to_string());
            let check = match (
                limit.check.as_deref(),
                limit.brand.as_deref(),
                limit.image.as_deref(),
                limit.os.as_deref(),
            ) {
                (Some("brand"), Some(v), _, _) => format!("brand={v}"),
                (Some("image"), _, Some(v), _) => format!("image={v}"),
                (Some("os"), _, _, Some(v)) => format!("os={v}"),
                _ => String::new(),
            };
            println!("{:<10} {:>7}  {:>5}  {}", by, limit.value, used, check);
        }
    }

    Ok(())
}

async fn update_account(
    args: AccountUpdateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();

    // Handle file-based input
    if let Some(file_path) = &args.file {
        let content = tokio::fs::read_to_string(file_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read file '{}': {}", file_path.display(), e))?;

        let request: triton_gateway_client::types::UpdateAccountRequest =
            serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse JSON: {}", e))?;

        let response = client
            .inner()
            .update_account()
            .account(account)
            .body(request)
            .send()
            .await?;
        let acc = response.into_inner();

        eprintln!("Account updated from file");

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

    let request = triton_gateway_client::types::UpdateAccountRequest {
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

    eprintln!("Account updated");

    if use_json {
        json::print_json(&acc)?;
    }

    Ok(())
}
