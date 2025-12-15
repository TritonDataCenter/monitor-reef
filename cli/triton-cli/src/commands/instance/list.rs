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

    /// Sort by field
    #[arg(long, default_value = "name")]
    pub sort_by: String,

    /// Custom output fields
    #[arg(short = 'o', long)]
    pub output: Option<String>,

    /// Show only short ID
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
    let mut tbl = table::create_table(&["SHORTID", "NAME", "IMAGE", "STATE", "PRIMARYIP", "AGE"]);

    for m in machines {
        let short_id = &m.id[..8.min(m.id.len())];
        let name = &m.name;
        let image = &m.image[..8.min(m.image.len())];
        let state = format!("{:?}", m.state).to_lowercase();
        let primary_ip = m.primary_ip.as_deref().unwrap_or("-");
        let age = format_age(&m.created);

        if args.short {
            println!("{}", short_id);
        } else {
            tbl.add_row(vec![short_id, name, image, &state, primary_ip, &age]);
        }
    }

    if !args.short {
        table::print_table(tbl);
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
