// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Package management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{json, table};

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

    if use_json {
        json::print_json(&packages)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "MEMORY", "DISK", "VCPUS"]);
        for pkg in &packages {
            let vcpus_str = if pkg.vcpus > 0 {
                pkg.vcpus.to_string()
            } else {
                "-".to_string()
            };
            tbl.add_row(vec![
                &pkg.id.to_string()[..8],
                &pkg.name,
                &format!("{} MB", pkg.memory),
                &format!("{} MB", pkg.disk),
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
        println!("ID:          {}", package.id);
        println!("Name:        {}", package.name);
        println!("Memory:      {} MB", package.memory);
        println!("Disk:        {} MB", package.disk);
        println!("Swap:        {} MB", package.swap);
        if package.vcpus > 0 {
            println!("vCPUs:       {}", package.vcpus);
        }
        if let Some(lwps) = package.lwps {
            println!("LWPs:        {}", lwps);
        }
        println!("Default:     {}", package.default);
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
