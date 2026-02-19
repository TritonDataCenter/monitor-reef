// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Package management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{format_mb, json, table};

/// Valid filter keys for positional key=value arguments
const VALID_FILTERS: &[&str] = &[
    "name", "memory", "disk", "swap", "lwps", "version", "vcpus", "group",
];

#[derive(Subcommand, Clone)]
pub enum PackageCommand {
    /// List packages
    #[command(visible_alias = "ls")]
    List(PackageListArgs),
    /// Get package details
    Get(PackageGetArgs),
}

#[derive(Args, Clone)]
pub struct PackageListArgs {
    /// Filter by name
    #[arg(long)]
    pub name: Option<String>,
    /// Filter by memory (MB)
    #[arg(long)]
    pub memory: Option<i64>,
    /// Filter by disk (MB)
    #[arg(long)]
    pub disk: Option<i64>,
    /// Filter by swap (MB)
    #[arg(long)]
    pub swap: Option<i64>,
    /// Filter by lwps (lightweight processes)
    #[arg(long)]
    pub lwps: Option<u32>,
    /// Filter by version
    #[arg(long)]
    pub version: Option<String>,
    /// Filter by vcpus
    #[arg(long)]
    pub vcpus: Option<u32>,
    /// Filter by group
    #[arg(long)]
    pub group: Option<String>,

    /// Filters in key=value format (e.g., name=base, memory=1024, group=g4)
    ///
    /// Supported filter keys: name, memory, disk, swap, lwps, version, vcpus, group
    #[arg(trailing_var_arg = true)]
    pub filters: Vec<String>,
}

#[derive(Args, Clone)]
pub struct PackageGetArgs {
    /// Package ID or name
    pub package: String,
}

impl PackageCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_packages(args, client, use_json).await,
            Self::Get(args) => get_package(args, client, use_json).await,
        }
    }
}

/// Apply positional key=value filters to the PackageListArgs, merging with any
/// existing --flag values. Positional filters override flags if both are set.
fn apply_positional_filters(args: &mut PackageListArgs) -> Result<()> {
    for filter in std::mem::take(&mut args.filters) {
        let (key, value) = filter
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid filter '{}': must be key=value", filter))?;

        if !VALID_FILTERS.contains(&key) {
            anyhow::bail!(
                "Unknown filter '{}'. Valid filters: {}",
                key,
                VALID_FILTERS.join(", ")
            );
        }

        match key {
            "name" => args.name = Some(value.to_string()),
            "memory" => {
                args.memory = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid memory value '{}': expected integer (MB)", value)
                })?);
            }
            "disk" => {
                args.disk = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid disk value '{}': expected integer (MB)", value)
                })?);
            }
            "swap" => {
                args.swap = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid swap value '{}': expected integer (MB)", value)
                })?);
            }
            "lwps" => {
                args.lwps = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid lwps value '{}': expected integer", value)
                })?);
            }
            "version" => args.version = Some(value.to_string()),
            "vcpus" => {
                args.vcpus = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid vcpus value '{}': expected integer", value)
                })?);
            }
            "group" => args.group = Some(value.to_string()),
            _ => unreachable!(),
        }
    }
    Ok(())
}

async fn list_packages(
    mut args: PackageListArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    apply_positional_filters(&mut args)?;

    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_packages()
        .account(account)
        .send()
        .await?;
    let all_packages = response.into_inner();

    // Client-side filtering
    let packages: Vec<_> = all_packages
        .into_iter()
        .filter(|pkg| {
            if let Some(name) = &args.name
                && !pkg.name.contains(name)
            {
                return false;
            }
            if let Some(memory) = args.memory
                && pkg.memory != memory as u64
            {
                return false;
            }
            if let Some(disk) = args.disk
                && pkg.disk != disk as u64
            {
                return false;
            }
            if let Some(swap) = args.swap
                && pkg.swap != swap as u64
            {
                return false;
            }
            if let Some(lwps) = args.lwps
                && pkg.lwps != Some(lwps)
            {
                return false;
            }
            if let Some(version) = &args.version
                && pkg.version.as_deref() != Some(version)
            {
                return false;
            }
            if let Some(vcpus) = args.vcpus
                && pkg.vcpus != vcpus
            {
                return false;
            }
            if let Some(group) = &args.group
                && pkg.group.as_deref() != Some(group)
            {
                return false;
            }
            true
        })
        .collect();

    // Sort by group (or name prefix), then memory to match node-triton behavior
    // node-triton's _groupPlus is: group || (name prefix before first '-') || ''
    let mut packages = packages;
    packages.sort_by(|a, b| {
        let group_a = a.group.as_deref().unwrap_or_else(|| {
            a.name
                .split('-')
                .next()
                .filter(|_| a.name.contains('-'))
                .unwrap_or("")
        });
        let group_b = b.group.as_deref().unwrap_or_else(|| {
            b.name
                .split('-')
                .next()
                .filter(|_| b.name.contains('-'))
                .unwrap_or("")
        });
        match group_a.cmp(group_b) {
            std::cmp::Ordering::Equal => a.memory.cmp(&b.memory),
            other => other,
        }
    });

    if use_json {
        json::print_json_stream(&packages)?;
    } else {
        // Columns: SHORTID(0), NAME(1), MEMORY(2), SWAP(3), DISK(4), VCPUS(5)
        // Right-align numeric columns to match node-triton
        let mut tbl = table::create_table_with_alignment(
            &["SHORTID", "NAME", "MEMORY", "SWAP", "DISK", "VCPUS"],
            &[2, 3, 4, 5], // MEMORY, SWAP, DISK, VCPUS
        );
        for pkg in &packages {
            let vcpus_str = if pkg.vcpus > 0 {
                pkg.vcpus.to_string()
            } else {
                "-".to_string()
            };
            tbl.add_row(vec![
                &pkg.id.to_string()[..8],
                &pkg.name,
                &format_mb(pkg.memory),
                &format_mb(pkg.swap),
                &format_mb(pkg.disk),
                &vcpus_str,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_package(args: PackageGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let package_id = resolve_package(&args.package, client).await?;

    let response = client
        .inner()
        .get_package()
        .account(account)
        .package(&package_id)
        .send()
        .await?;

    let package = response.into_inner();

    if use_json {
        json::print_json(&package)?;
    } else {
        json::print_json_pretty(&package)?;
    }

    Ok(())
}

/// Resolve package name or short ID to full UUID
pub async fn resolve_package(id_or_name: &str, client: &TypedClient) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_packages()
        .account(account)
        .send()
        .await?;

    let packages = response.into_inner();

    // Try short ID match first (at least 8 characters)
    if id_or_name.len() >= 8 {
        for pkg in &packages {
            if pkg.id.to_string().starts_with(id_or_name) {
                return Ok(pkg.id.to_string());
            }
        }
    }

    // Try exact name match
    for pkg in &packages {
        if pkg.name == id_or_name {
            return Ok(pkg.id.to_string());
        }
    }

    Err(crate::errors::ResourceNotFoundError(format!("Package not found: {}", id_or_name)).into())
}
