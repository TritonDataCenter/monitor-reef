// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Package management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_api::Package;

use crate::client::AnyClient;
use crate::define_columns;
use crate::dispatch;
use crate::output::table::{TableBuilder, TableFormatArgs};
use crate::output::{format_mb, json};

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

    #[command(flatten)]
    pub table: TableFormatArgs,

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
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
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

/// List all packages visible to the caller.
///
/// Fetches once from the dispatched client, converts per-client
/// `Package` Progenitor types into the canonical `cloudapi_api::Package`
/// via a JSON round-trip, then applies client-side filtering/sorting
/// against the canonical type.
async fn list_packages(
    mut args: PackageListArgs,
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    apply_positional_filters(&mut args)?;

    let account = client.effective_account();

    let all_packages: Vec<Package> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_packages()
            .account(account)
            .send()
            .await?
            .into_inner();
        let value = serde_json::to_value(&resp)?;
        serde_json::from_value::<Vec<Package>>(value)?
    });

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
        define_columns! {
            PackageColumn for Package, long_from: 6, {
                ShortId("SHORTID") => |pkg| {
                    let id_str = pkg.id.to_string();
                    id_str[..8.min(id_str.len())].to_string()
                },
                Name("NAME") => |pkg| pkg.name.clone(),
                Memory("MEMORY") => |pkg| format_mb(pkg.memory),
                Swap("SWAP") => |pkg| format_mb(pkg.swap),
                Disk("DISK") => |pkg| format_mb(pkg.disk),
                Vcpus("VCPUS") => |pkg| {
                    if pkg.vcpus > 0 { pkg.vcpus.to_string() } else { "-".to_string() }
                },
                // --- long-only columns below ---
                Id("ID") => |pkg| pkg.id.to_string(),
                Description("DESCRIPTION") => |pkg| {
                    pkg.description.clone().unwrap_or_else(|| "-".to_string())
                },
                Version("VERSION") => |pkg| {
                    pkg.version.clone().unwrap_or_else(|| "-".to_string())
                },
                Group("GROUP") => |pkg| {
                    pkg.group.clone().unwrap_or_else(|| "-".to_string())
                },
                Lwps("LWPS") => |pkg| {
                    pkg.lwps.map_or("-".to_string(), |v| v.to_string())
                },
                Default("DEFAULT") => |pkg| pkg.default.to_string(),
            }
        }

        TableBuilder::from_enum_columns::<PackageColumn, _>(
            &packages,
            Some(PackageColumn::LONG_FROM),
        )
        .with_right_aligned(&["MEMORY", "SWAP", "DISK", "VCPUS", "LWPS"])
        .print(&args.table)?;
    }

    Ok(())
}

async fn get_package(args: PackageGetArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let package_id = resolve_package(&args.package, client).await?;

    let package_json: serde_json::Value = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_package()
            .account(account)
            .package(&package_id)
            .send()
            .await?
            .into_inner();
        serde_json::to_value(&resp)?
    });

    if use_json {
        json::print_json(&package_json)?;
    } else {
        json::print_json_pretty(&package_json)?;
    }

    Ok(())
}

/// Resolve package name or short ID to full UUID
pub async fn resolve_package(id_or_name: &str, client: &AnyClient) -> Result<String> {
    let account = client.effective_account();

    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        // Verify the package exists (matches node-triton's getPackage call)
        // In emit-payload mode, the exec hook returns a fake response
        dispatch!(client, |c| {
            c.inner()
                .get_package()
                .account(account)
                .package(uuid.to_string())
                .send()
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;
        return Ok(uuid.to_string());
    }

    // Fetch the package list once and match by short-ID or exact name.
    let packages: Vec<(String, String)> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_packages()
            .account(account)
            .send()
            .await?
            .into_inner();
        resp.into_iter()
            .map(|p| (p.id.to_string(), p.name))
            .collect()
    });

    // Try short ID match first (at least 8 characters)
    if id_or_name.len() >= 8 {
        for (id, _) in &packages {
            if id.starts_with(id_or_name) {
                return Ok(id.clone());
            }
        }
    }

    // Try exact name match
    for (id, name) in &packages {
        if name == id_or_name {
            return Ok(id.clone());
        }
    }

    Err(crate::errors::ResourceNotFoundError(format!("Package not found: {}", id_or_name)).into())
}
