// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance list command

use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::Args;
use cloudapi_client::TypedClient;
use cloudapi_client::types::Machine;

use crate::output::{json, table};

#[derive(Args, Clone)]
pub struct ListArgs {
    /// Filter by name (substring match)
    #[arg(long)]
    pub name: Option<String>,

    /// Filter by state
    #[arg(long)]
    pub state: Option<String>,

    /// Filter by image
    #[arg(long)]
    pub image: Option<String>,

    /// Filter by package
    #[arg(long)]
    pub package: Option<String>,

    /// Filter by tag (key=value)
    #[arg(long, short = 't')]
    pub tag: Option<Vec<String>>,

    /// Maximum results
    #[arg(long)]
    pub limit: Option<i64>,

    /// Sort by field (created, name, state, etc.)
    #[arg(long, short = 's', default_value = "name")]
    pub sort_by: String,

    /// Custom output fields (comma-separated)
    #[arg(short = 'o', long)]
    pub output: Option<String>,

    /// Long output format with more columns
    #[arg(short = 'l', long)]
    pub long: bool,

    /// Omit table header
    #[arg(short = 'H', long = "no-header")]
    pub no_header: bool,

    /// Show only short ID (one per line)
    #[arg(long)]
    pub short: bool,
}

pub async fn run(args: ListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let mut req = client.inner().list_machines().account(account);

    if let Some(name) = &args.name {
        req = req.name(name);
    }
    if let Some(state) = &args.state {
        req = req.state(state);
    }
    if let Some(image) = &args.image {
        req = req.image(image);
    }
    // Note: package filter may need to be done client-side if not supported by API
    // if let Some(pkg) = &args.package {
    //     req = req.package(pkg);
    // }
    if let Some(limit) = args.limit {
        req = req.limit(limit);
    }
    // Handle tags - CloudAPI uses tag.key=value format
    if let Some(tags) = &args.tag {
        for tag in tags {
            if let Some((key, value)) = tag.split_once('=') {
                req = req.tag(format!("tag.{}={}", key, value));
            }
        }
    }

    let response = req.send().await?;
    let machines = response.into_inner();

    if use_json {
        json::print_json(&machines)?;
    } else {
        print_machines_table(&machines, &args);
    }

    Ok(())
}

fn print_machines_table(machines: &[Machine], args: &ListArgs) {
    // Handle --short: just print IDs
    if args.short {
        for m in machines {
            let short_id = &m.id[..8.min(m.id.len())];
            println!("{}", short_id);
        }
        return;
    }

    // Determine columns based on --long or --output
    let columns: Vec<&str> = if let Some(ref output) = args.output {
        output.split(',').map(|s| s.trim()).collect()
    } else if args.long {
        vec![
            "id",
            "name",
            "img",
            "brand",
            "package",
            "state",
            "flags",
            "primaryIp",
            "created",
        ]
    } else {
        vec!["shortid", "name", "img", "state", "flags", "age"]
    };

    // Create header (uppercase)
    let headers: Vec<String> = columns.iter().map(|c| c.to_uppercase()).collect();
    let header_refs: Vec<&str> = headers.iter().map(|s| s.as_str()).collect();

    let mut tbl = if args.no_header {
        table::create_table_no_header(columns.len())
    } else {
        table::create_table(&header_refs)
    };

    for m in machines {
        let row: Vec<String> = columns.iter().map(|col| get_field_value(m, col)).collect();
        let row_refs: Vec<&str> = row.iter().map(|s| s.as_str()).collect();
        tbl.add_row(row_refs);
    }

    table::print_table(tbl);
}

/// Get a field value from a Machine by field name
fn get_field_value(m: &Machine, field: &str) -> String {
    match field.to_lowercase().as_str() {
        "id" => m.id.clone(),
        "shortid" => m.id[..8.min(m.id.len())].to_string(),
        "name" => m.name.clone(),
        "image" => m.image.clone(),
        "img" => m.image[..8.min(m.image.len())].to_string(),
        "state" => format!("{:?}", m.state).to_lowercase(),
        "brand" => format!("{:?}", m.brand).to_lowercase(),
        "package" => m.package.clone(),
        "memory" => m.memory.to_string(),
        "disk" => m.disk.to_string(),
        "primaryip" => m.primary_ip.clone().unwrap_or_else(|| "-".to_string()),
        "created" => m.created.clone(),
        "age" => format_age(&m.created),
        "flags" => {
            let mut flags = Vec::new();
            let brand_str = format!("{:?}", m.brand).to_lowercase();
            if brand_str == "bhyve" {
                flags.push('B');
            }
            if m.docker.unwrap_or(false) {
                flags.push('D');
            }
            if m.firewall_enabled.unwrap_or(false) {
                flags.push('F');
            }
            if brand_str == "kvm" {
                flags.push('K');
            }
            if m.deletion_protection.unwrap_or(false) {
                flags.push('P');
            }
            if flags.is_empty() {
                "-".to_string()
            } else {
                flags.into_iter().collect()
            }
        }
        _ => "-".to_string(),
    }
}

fn format_age(created: &str) -> String {
    // Try to parse as RFC 3339
    if let Ok(created_dt) = DateTime::parse_from_rfc3339(created) {
        let now = Utc::now();
        let created_utc = created_dt.with_timezone(&Utc);
        let duration = now.signed_duration_since(created_utc);

        if duration.num_days() >= 365 {
            format!("{}y", duration.num_days() / 365)
        } else if duration.num_days() >= 30 {
            format!("{}mo", duration.num_days() / 30)
        } else if duration.num_days() >= 1 {
            format!("{}d", duration.num_days())
        } else if duration.num_hours() >= 1 {
            format!("{}h", duration.num_hours())
        } else if duration.num_minutes() >= 1 {
            format!("{}m", duration.num_minutes())
        } else {
            "now".to_string()
        }
    } else {
        "-".to_string()
    }
}
