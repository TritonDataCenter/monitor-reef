// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance list command

use std::collections::HashMap;

use anyhow::Result;
use clap::Args;
use serde::Serialize;
use triton_gateway_client::TypedClient;
use triton_gateway_client::pagination::{DEFAULT_PAGE_SIZE, paginate_all};
use triton_gateway_client::types::{Machine, VmBrand};

use crate::output::table::{TableBuilder, TableFormatArgs, col};
use crate::output::{self, enum_to_display, json, parse_filter_enum};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<String>,
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
            if m.brand == VmBrand::Bhyve {
                flags.push('B');
            }
            if m.docker.unwrap_or(false) {
                flags.push('D');
            }
            if m.firewall_enabled.unwrap_or(false) {
                flags.push('F');
            }
            if m.brand == VmBrand::Kvm {
                flags.push('K');
            }
            if m.deletion_protection.unwrap_or(false) {
                flags.push('P');
            }
            if flags.is_empty() {
                None
            } else {
                Some(flags.into_iter().collect())
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
    #[arg(long, value_enum)]
    pub state: Option<triton_gateway_client::types::MachineState>,

    /// Filter by image
    #[arg(long)]
    pub image: Option<String>,

    /// Filter by package
    #[arg(long)]
    pub package: Option<String>,

    /// Filter by brand
    #[arg(long, value_enum)]
    pub brand: Option<triton_gateway_client::types::VmBrand>,

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
    pub limit: Option<u64>,

    #[command(flatten)]
    pub table: TableFormatArgs,

    /// Show only short ID (one per line)
    #[arg(long)]
    pub short: bool,

    /// Include generated credentials in output (metadata.credentials)
    #[arg(long)]
    pub credentials: bool,

    /// Filters in key=value format (e.g., name=lb, state=running, tag.foo=bar)
    ///
    /// Supported filter keys: brand, docker, image, memory, name, state, type
    #[arg(trailing_var_arg = true)]
    pub filters: Vec<String>,
}

/// Valid filter keys for positional key=value arguments
const VALID_FILTERS: &[&str] = &[
    "brand", "docker", "image", "memory", "name", "state", "type",
];

/// Check if a filter key is valid (exact match or tag.* pattern)
fn is_valid_filter(key: &str) -> bool {
    VALID_FILTERS.contains(&key) || key.starts_with("tag.")
}

/// Apply positional key=value filters to the ListArgs, merging with any
/// existing --flag values. Positional filters override flags if both are set.
/// Returns an optional MachineType filter (from `type=` positional arg).
fn apply_positional_filters(
    args: &mut ListArgs,
) -> Result<Option<triton_gateway_client::types::MachineType>> {
    let mut machine_type = None;
    for filter in std::mem::take(&mut args.filters) {
        let (key, value) = filter
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid filter '{}': must be key=value", filter))?;

        if !is_valid_filter(key) {
            anyhow::bail!(
                "Unknown filter '{}'. Valid filters: {}, tag.*",
                key,
                VALID_FILTERS.join(", ")
            );
        }

        match key {
            "name" => args.name = Some(value.to_string()),
            "state" => {
                args.state = Some(parse_filter_enum("state", value)?);
            }
            "image" => args.image = Some(value.to_string()),
            "brand" => {
                args.brand = Some(parse_filter_enum("brand", value)?);
            }
            "memory" => {
                args.memory = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid memory value '{}': expected integer (MB)", value)
                })?);
            }
            "docker" => {
                args.docker = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid docker value '{}': expected true or false", value)
                })?);
            }
            "type" => {
                machine_type = Some(parse_filter_enum("type", value)?);
            }
            _ if key.starts_with("tag.") => {
                let tag_key = &key["tag.".len()..];
                let tag_entry = format!("{}={}", tag_key, value);
                match &mut args.tag {
                    Some(tags) => tags.push(tag_entry),
                    None => args.tag = Some(vec![tag_entry]),
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(machine_type)
}

pub async fn run(
    mut args: ListArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let machine_type = apply_positional_filters(&mut args)?;

    let account = client.effective_account();

    // Parse image UUID up front so we can report errors before starting pagination
    let image_uuid = match &args.image {
        Some(image) => Some(
            image
                .parse::<uuid::Uuid>()
                .map_err(|_| anyhow::anyhow!("Invalid image UUID: {}", image))?,
        ),
        None => None,
    };

    // Clone filter values that need to move into the pagination closure
    let name = args.name.clone();
    let state = args.state;
    let brand = args.brand;
    let memory = args.memory;
    let docker = args.docker;
    let credentials = args.credentials;
    let tags = args.tag.clone();
    let max_results = args.limit;

    let fetch_machines = paginate_all(DEFAULT_PAGE_SIZE, max_results, |limit, offset| {
        let account = account.to_string();
        let name = name.clone();
        let tags = tags.clone();
        async move {
            let mut req = client.inner().list_machines().account(&account);

            if let Some(name) = &name {
                req = req.name(name);
            }
            if let Some(state) = state {
                req = req.state(state);
            }
            if let Some(image) = image_uuid {
                req = req.image(image);
            }
            if let Some(brand) = brand {
                req = req.brand(brand);
            }
            if let Some(memory) = memory {
                req = req.memory(memory as i64);
            }
            if let Some(docker) = docker {
                req = req.docker(docker);
            }
            if credentials {
                req = req.credentials(true);
            }
            if let Some(mt) = machine_type {
                req = req.type_(mt);
            }
            // Handle tags - CloudAPI uses tag.key=value format
            if let Some(tags) = &tags {
                for tag in tags {
                    if let Some((key, value)) = tag.split_once('=') {
                        req = req.tag(format!("tag.{}={}", key, value));
                    }
                }
            }

            req = req.limit(limit).offset(offset);

            let resp = req.send().await?;
            Ok::<_, triton_gateway_client::Error<triton_gateway_client::types::Error>>(
                resp.into_inner(),
            )
        }
    });

    // Try loading images from cache first to avoid a parallel API call
    let cached_images = match cache {
        Some(c) => c.load_list().await,
        None => None,
    };

    let (machines, image_map) = if let Some(images) = cached_images {
        // Cache hit — only fetch machines (skip images API call)
        let machines = fetch_machines.await?;
        let map: HashMap<uuid::Uuid, String> = images
            .into_iter()
            .map(|img| (img.id, format!("{}@{}", img.name, img.version)))
            .collect();
        (machines, map)
    } else {
        // Cache miss — fetch machines and images in parallel
        let images_req = client
            .inner()
            .list_images()
            .account(account)
            .state(triton_gateway_client::types::ImageState::All)
            .send();
        let (machines_result, images_response) = tokio::join!(fetch_machines, images_req);
        let machines = machines_result?;
        let map: HashMap<uuid::Uuid, String> = match images_response {
            Ok(r) => {
                let images = r.into_inner();
                if let Some(c) = cache {
                    c.save_list(&images).await;
                }
                images
                    .into_iter()
                    .map(|img| (img.id, format!("{}@{}", img.name, img.version)))
                    .collect()
            }
            Err(e) => {
                tracing::warn!("failed to fetch images: {}", e);
                HashMap::new()
            }
        };
        (machines, map)
    };

    if use_json {
        let augmented: Vec<AugmentedMachine> = machines
            .iter()
            .map(|m| AugmentedMachine::from_machine(m, &image_map))
            .collect();
        json::print_json_stream(&augmented)?;
    } else {
        print_machines_table(&machines, &args, &image_map)?;
    }

    Ok(())
}

fn print_machines_table(
    machines: &[Machine],
    args: &ListArgs,
    image_map: &HashMap<uuid::Uuid, String>,
) -> Result<()> {
    // Handle --short: just print IDs
    if args.short {
        for m in machines {
            let id_str = m.id.to_string();
            let short_id = &id_str[..8.min(id_str.len())];
            println!("{}", short_id);
        }
        return Ok(());
    }

    // Sort by created (ascending) by default when no -s flag is provided,
    // matching node-triton's tabula default sort direction.
    let mut sorted_machines: Vec<&Machine> = machines.iter().collect();
    if args.table.sort_by.is_none() {
        sorted_machines.sort_by(|a, b| a.created.cmp(&b.created));
    }

    let columns = vec![
        col("SHORTID", |m: &&Machine| {
            let id_str = m.id.to_string();
            id_str[..8.min(id_str.len())].to_string()
        }),
        col("NAME", |m: &&Machine| m.name.clone()),
        col("IMG", |m: &&Machine| {
            image_map.get(&m.image).cloned().unwrap_or_else(|| {
                let image_str = m.image.to_string();
                image_str[..8.min(image_str.len())].to_string()
            })
        }),
        col("STATE", |m: &&Machine| enum_to_display(&m.state)),
        col("FLAGS", |m: &&Machine| {
            let mut flags = Vec::new();
            if m.brand == VmBrand::Bhyve {
                flags.push('B');
            }
            if m.docker.unwrap_or(false) {
                flags.push('D');
            }
            if m.firewall_enabled.unwrap_or(false) {
                flags.push('F');
            }
            if m.brand == VmBrand::Kvm {
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
        }),
        col("AGE", |m: &&Machine| output::format_age(&m.created)),
        col("ID", |m: &&Machine| m.id.to_string()),
        col("BRAND", |m: &&Machine| enum_to_display(&m.brand)),
        col("PACKAGE", |m: &&Machine| m.package.clone()),
        col("PRIMARYIP", |m: &&Machine| {
            m.primary_ip.clone().unwrap_or_else(|| "-".to_string())
        }),
        col("CREATED", |m: &&Machine| m.created.to_rfc3339()),
    ];

    // Set default columns based on short/long mode to match node-triton.
    // All columns remain available for -o selection (long_from: None).
    let mut table_opts = args.table.clone();
    if table_opts.columns.is_none() {
        table_opts.columns = Some(
            if table_opts.long {
                vec![
                    "ID",
                    "NAME",
                    "IMG",
                    "BRAND",
                    "PACKAGE",
                    "STATE",
                    "FLAGS",
                    "PRIMARYIP",
                    "CREATED",
                ]
            } else {
                vec!["SHORTID", "NAME", "IMG", "STATE", "FLAGS", "AGE"]
            }
            .into_iter()
            .map(String::from)
            .collect(),
        );
    }

    TableBuilder::from_columns(&columns, &sorted_machines, None)
        .with_right_aligned(&["MEMORY"])
        .print(&table_opts)?;
    Ok(())
}
