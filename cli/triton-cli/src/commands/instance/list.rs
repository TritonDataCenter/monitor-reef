// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance list command

use std::collections::HashMap;

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use cloudapi_client::types::Machine;
use serde::Serialize;

use crate::output::{self, json, table};

/// Augmented machine output with computed fields for node-triton compatibility
#[derive(Serialize)]
struct AugmentedMachine {
    #[serde(flatten)]
    machine: Machine,
    /// Short ID (first 8 chars of UUID)
    shortid: String,
    /// Image name@version
    img: String,
    /// Instance flags (B=bhyve, D=docker, F=firewall, K=kvm, P=deletion_protection)
    flags: String,
    /// Age of the instance
    age: String,
}

impl AugmentedMachine {
    fn from_machine(m: &Machine, image_map: &HashMap<uuid::Uuid, String>) -> Self {
        let id_str = m.id.to_string();
        let shortid = id_str[..8.min(id_str.len())].to_string();

        let img = image_map.get(&m.image).cloned().unwrap_or_else(|| {
            let image_str = m.image.to_string();
            image_str[..8.min(image_str.len())].to_string()
        });

        let flags = {
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
        };

        let age = output::format_age(&m.created);

        AugmentedMachine {
            machine: m.clone(),
            shortid,
            img,
            flags,
            age,
        }
    }
}

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

    /// Filter by brand (joyent, lx, bhyve, kvm)
    #[arg(long)]
    pub brand: Option<String>,

    /// Filter by memory size in MB
    #[arg(long)]
    pub memory: Option<u64>,

    /// Filter by docker flag (true/false)
    #[arg(long)]
    pub docker: Option<bool>,

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

    /// Include generated credentials in output (metadata.credentials)
    #[arg(long)]
    pub credentials: bool,
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
        let image_uuid: uuid::Uuid = image
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid image UUID: {}", image))?;
        req = req.image(image_uuid);
    }
    // Note: package filter may need to be done client-side if not supported by API
    // if let Some(pkg) = &args.package {
    //     req = req.package(pkg);
    // }
    if let Some(brand) = &args.brand {
        req = req.brand(brand);
    }
    if let Some(memory) = args.memory {
        req = req.memory(memory as i64);
    }
    if let Some(docker) = args.docker {
        req = req.docker(docker);
    }
    if args.credentials {
        req = req.credentials(true);
    }
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

    // Fetch machines and images in parallel for performance
    let images_req = client.inner().list_images().account(account).send();
    let (machines_response, images_response) = tokio::join!(req.send(), images_req);

    let machines = machines_response?.into_inner();

    // Build image UUID -> name@version map
    let image_map: HashMap<uuid::Uuid, String> = images_response
        .map(|r| {
            r.into_inner()
                .into_iter()
                .map(|img| (img.id, format!("{}@{}", img.name, img.version)))
                .collect()
        })
        .unwrap_or_default();

    if use_json {
        // Augment machines with computed fields for node-triton compatibility
        let augmented: Vec<AugmentedMachine> = machines
            .iter()
            .map(|m| AugmentedMachine::from_machine(m, &image_map))
            .collect();
        json::print_json_stream(&augmented)?;
    } else {
        print_machines_table(&machines, &args, &image_map);
    }

    Ok(())
}

fn print_machines_table(
    machines: &[Machine],
    args: &ListArgs,
    image_map: &HashMap<uuid::Uuid, String>,
) {
    // Handle --short: just print IDs
    if args.short {
        for m in machines {
            let id_str = m.id.to_string();
            let short_id = &id_str[..8.min(id_str.len())];
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
        let row: Vec<String> = columns
            .iter()
            .map(|col| get_field_value(m, col, image_map))
            .collect();
        let row_refs: Vec<&str> = row.iter().map(|s| s.as_str()).collect();
        tbl.add_row(row_refs);
    }

    table::print_table(tbl);
}

/// Get a field value from a Machine by field name
fn get_field_value(m: &Machine, field: &str, image_map: &HashMap<uuid::Uuid, String>) -> String {
    match field.to_lowercase().as_str() {
        "id" => m.id.to_string(),
        "shortid" => {
            let id_str = m.id.to_string();
            id_str[..8.min(id_str.len())].to_string()
        }
        "name" => m.name.clone(),
        "image" => m.image.to_string(),
        "img" => {
            // Look up image name@version from map, fall back to short UUID
            image_map.get(&m.image).cloned().unwrap_or_else(|| {
                let image_str = m.image.to_string();
                image_str[..8.min(image_str.len())].to_string()
            })
        }
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
    output::format_age(created)
}
