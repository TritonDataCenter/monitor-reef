// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Package management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{format_mb, json, table};

#[derive(Subcommand, Clone)]
pub enum PackageCommand {
    /// List packages
    #[command(alias = "ls")]
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

async fn list_packages(args: PackageListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
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

    // node-triton outputs JSON by default for package get:
    // - Without -j: pretty-printed JSON (4-space indent)
    // - With -j: compact JSON (single line)
    if use_json {
        // Compact JSON (single line)
        println!("{}", serde_json::to_string(&package)?);
    } else {
        // Pretty-printed JSON (default)
        json::print_json(&package)?;
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

    Err(anyhow::anyhow!("Package not found: {}", id_or_name))
}
