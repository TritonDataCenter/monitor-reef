// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! PAPI CLI - Command-line interface for Triton PAPI (Packages API)
//!
//! This CLI provides access to all PAPI endpoints for managing package
//! definitions (RAM, CPU, disk, etc.) used by other services to provision VMs.
//!
//! # Environment Variables
//!
//! - `PAPI_URL` - PAPI base URL (default: http://localhost)

use anyhow::Result;
use clap::{Parser, Subcommand};
use uuid::Uuid;

use papi_client::Client;
use papi_client::types;

/// Convert a serde-serializable enum value to its wire-format string.
fn enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", val))
}

#[derive(Parser)]
#[command(name = "papi", version, about = "CLI for Triton PAPI (Packages API)")]
struct Cli {
    /// PAPI base URL
    #[arg(long, env = "PAPI_URL", default_value = "http://localhost")]
    base_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // ========================================================================
    // Ping
    // ========================================================================
    /// Health check endpoint
    Ping,

    // ========================================================================
    // Packages - List
    // ========================================================================
    /// List packages
    #[command(name = "list")]
    List {
        /// Filter by package name
        #[arg(long)]
        name: Option<String>,
        /// Filter by version
        #[arg(long)]
        version: Option<String>,
        /// Filter by active status
        #[arg(long)]
        active: Option<bool>,
        /// Filter by brand
        #[arg(long, value_enum)]
        brand: Option<types::Brand>,
        /// Filter by owner UUIDs
        #[arg(long)]
        owner_uuids: Option<String>,
        /// Filter by group
        #[arg(long)]
        group: Option<String>,
        /// Filter by OS
        #[arg(long)]
        os: Option<String>,
        /// Filter by flexible_disk
        #[arg(long)]
        flexible_disk: Option<bool>,
        /// Raw LDAP search filter
        #[arg(long)]
        filter: Option<String>,
        /// Field name to sort by
        #[arg(long)]
        sort: Option<String>,
        /// Sort order
        #[arg(long, value_enum)]
        order: Option<types::SortOrder>,
        /// Limit result count
        #[arg(long)]
        limit: Option<u64>,
        /// Skip N results
        #[arg(long)]
        offset: Option<u64>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Packages - Get
    // ========================================================================
    /// Get a package by UUID
    #[command(name = "get")]
    Get {
        /// Package UUID
        uuid: Uuid,
        /// Owner UUIDs for access filtering
        #[arg(long)]
        owner_uuids: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Packages - Create
    // ========================================================================
    /// Create a new package
    #[command(name = "create")]
    Create {
        /// Package name
        #[arg(long)]
        name: String,
        /// Semver version string
        #[arg(long)]
        version: String,
        /// Whether the package is active for provisioning
        #[arg(long)]
        active: bool,
        /// Maximum number of lightweight processes (min: 250)
        #[arg(long)]
        max_lwps: u64,
        /// Maximum physical memory in MiB (min: 64)
        #[arg(long)]
        max_physical_memory: u64,
        /// Maximum swap in MiB (min: 128)
        #[arg(long)]
        max_swap: u64,
        /// Disk quota in MiB (min: 1024, multiple of 1024)
        #[arg(long)]
        quota: u64,
        /// ZFS I/O priority (0..16383)
        #[arg(long)]
        zfs_io_priority: u64,
        /// Package UUID (auto-generated if not provided)
        #[arg(long)]
        uuid: Option<Uuid>,
        /// CPU cap value
        #[arg(long)]
        cpu_cap: Option<u64>,
        /// VM brand
        #[arg(long, value_enum)]
        brand: Option<types::Brand>,
        /// Owner UUIDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        owner_uuids: Option<Vec<Uuid>>,
        /// Number of virtual CPUs (1..64)
        #[arg(long)]
        vcpus: Option<u64>,
        /// Package grouping
        #[arg(long)]
        group: Option<String>,
        /// Human-readable description
        #[arg(long)]
        description: Option<String>,
        /// Display name in portal
        #[arg(long)]
        common_name: Option<String>,
        /// Operating system
        #[arg(long)]
        os: Option<String>,
        /// Parent package name or UUID
        #[arg(long)]
        parent: Option<String>,
        /// CPU shares
        #[arg(long)]
        fss: Option<u64>,
        /// CPU burst ratio
        #[arg(long)]
        cpu_burst_ratio: Option<f64>,
        /// RAM ratio
        #[arg(long)]
        ram_ratio: Option<f64>,
        /// Billing tag
        #[arg(long)]
        billing_tag: Option<String>,
        /// Server allocation spread strategy
        #[arg(long, value_enum)]
        alloc_server_spread: Option<types::AllocServerSpread>,
        /// Enable flexible disk mode (bhyve only)
        #[arg(long)]
        flexible_disk: Option<bool>,
        /// Skip validation step
        #[arg(long)]
        skip_validation: Option<bool>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Packages - Update
    // ========================================================================
    /// Update a package
    #[command(name = "update")]
    Update {
        /// Package UUID
        uuid: Uuid,
        /// Whether the package is active for provisioning
        #[arg(long)]
        active: Option<bool>,
        /// Owner UUIDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        owner_uuids: Option<Vec<Uuid>>,
        /// Package grouping
        #[arg(long)]
        group: Option<String>,
        /// Human-readable description
        #[arg(long)]
        description: Option<String>,
        /// Display name in portal
        #[arg(long)]
        common_name: Option<String>,
        /// Parent package name or UUID
        #[arg(long)]
        parent: Option<String>,
        /// CPU shares
        #[arg(long)]
        fss: Option<u64>,
        /// CPU burst ratio
        #[arg(long)]
        cpu_burst_ratio: Option<f64>,
        /// RAM ratio
        #[arg(long)]
        ram_ratio: Option<f64>,
        /// Billing tag
        #[arg(long)]
        billing_tag: Option<String>,
        /// Server allocation spread strategy
        #[arg(long, value_enum)]
        alloc_server_spread: Option<types::AllocServerSpread>,
        /// Enable flexible disk mode (bhyve only)
        #[arg(long)]
        flexible_disk: Option<bool>,
        /// Allow modifying immutable fields
        #[arg(long)]
        force: Option<bool>,
        /// Skip validation step
        #[arg(long)]
        skip_validation: Option<bool>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Packages - Delete
    // ========================================================================
    /// Delete a package (requires --force)
    #[command(name = "delete")]
    Delete {
        /// Package UUID
        uuid: Uuid,
        /// Required: must be true to allow deletion
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(&cli.base_url);

    match cli.command {
        Commands::Ping => {
            let resp = client
                .ping()
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Ping failed: {}", e))?;
            let ping = resp.into_inner();
            println!("{}", serde_json::to_string_pretty(&ping)?);
        }

        Commands::List {
            name,
            version,
            active,
            brand,
            owner_uuids,
            group,
            os,
            flexible_disk,
            filter,
            sort,
            order,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_packages();
            if let Some(v) = name {
                req = req.name(v);
            }
            if let Some(v) = version {
                req = req.version(v);
            }
            if let Some(v) = active {
                req = req.active(v);
            }
            if let Some(v) = brand {
                req = req.brand(v);
            }
            if let Some(v) = owner_uuids {
                req = req.owner_uuids(v);
            }
            if let Some(v) = group {
                req = req.group(v);
            }
            if let Some(v) = os {
                req = req.os(v);
            }
            if let Some(v) = flexible_disk {
                req = req.flexible_disk(v);
            }
            if let Some(v) = filter {
                req = req.filter(v);
            }
            if let Some(v) = sort {
                req = req.sort(v);
            }
            if let Some(v) = order {
                req = req.order(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List packages failed: {}", e))?;
            let packages = resp.into_inner();

            if raw {
                println!("{}", serde_json::to_string_pretty(&packages)?);
            } else {
                for pkg in &packages {
                    let brand_str = pkg.brand.as_ref().map(enum_to_display).unwrap_or_default();
                    println!(
                        "{} {} v{} [{}] active={} mem={}MiB",
                        pkg.uuid,
                        pkg.name,
                        pkg.version,
                        brand_str,
                        pkg.active,
                        pkg.max_physical_memory,
                    );
                }
                println!("({} packages)", packages.len());
            }
        }

        Commands::Get {
            uuid,
            owner_uuids,
            raw,
        } => {
            let mut req = client.get_package().uuid(uuid);
            if let Some(v) = owner_uuids {
                req = req.owner_uuids(v);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get package failed: {}", e))?;
            let pkg = resp.into_inner();

            if raw {
                println!("{}", serde_json::to_string_pretty(&pkg)?);
            } else {
                print_package_detail(&pkg);
            }
        }

        Commands::Create {
            name,
            version,
            active,
            max_lwps,
            max_physical_memory,
            max_swap,
            quota,
            zfs_io_priority,
            uuid,
            cpu_cap,
            brand,
            owner_uuids,
            vcpus,
            group,
            description,
            common_name,
            os,
            parent,
            fss,
            cpu_burst_ratio,
            ram_ratio,
            billing_tag,
            alloc_server_spread,
            flexible_disk,
            skip_validation,
            raw,
        } => {
            let mut builder = client.create_package().body_map(|b| {
                let mut b = b
                    .name(name)
                    .version(version)
                    .active(active)
                    .max_lwps(max_lwps)
                    .max_physical_memory(max_physical_memory)
                    .max_swap(max_swap)
                    .quota(quota)
                    .zfs_io_priority(zfs_io_priority);
                if let Some(v) = uuid {
                    b = b.uuid(v);
                }
                if let Some(v) = cpu_cap {
                    b = b.cpu_cap(v);
                }
                if let Some(v) = brand {
                    b = b.brand(v);
                }
                if let Some(v) = owner_uuids {
                    b = b.owner_uuids(v);
                }
                if let Some(v) = vcpus {
                    b = b.vcpus(v);
                }
                if let Some(v) = group {
                    b = b.group(v);
                }
                if let Some(v) = description {
                    b = b.description(v);
                }
                if let Some(v) = common_name {
                    b = b.common_name(v);
                }
                if let Some(v) = os {
                    b = b.os(v);
                }
                if let Some(v) = parent {
                    b = b.parent(v);
                }
                if let Some(v) = fss {
                    b = b.fss(v);
                }
                if let Some(v) = cpu_burst_ratio {
                    b = b.cpu_burst_ratio(v);
                }
                if let Some(v) = ram_ratio {
                    b = b.ram_ratio(v);
                }
                if let Some(v) = billing_tag {
                    b = b.billing_tag(v);
                }
                if let Some(v) = alloc_server_spread {
                    b = b.alloc_server_spread(v);
                }
                if let Some(v) = flexible_disk {
                    b = b.flexible_disk(v);
                }
                if let Some(v) = skip_validation {
                    b = b.skip_validation(v);
                }
                b
            });

            // Workaround: builder is consumed by body_map, reassign
            let _ = &mut builder;

            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create package failed: {}", e))?;
            let pkg = resp.into_inner();

            if raw {
                println!("{}", serde_json::to_string_pretty(&pkg)?);
            } else {
                print_package_detail(&pkg);
            }
        }

        Commands::Update {
            uuid,
            active,
            owner_uuids,
            group,
            description,
            common_name,
            parent,
            fss,
            cpu_burst_ratio,
            ram_ratio,
            billing_tag,
            alloc_server_spread,
            flexible_disk,
            force,
            skip_validation,
            raw,
        } => {
            let builder = client.update_package().uuid(uuid).body_map(|b| {
                let mut b = b;
                if let Some(v) = active {
                    b = b.active(v);
                }
                if let Some(v) = owner_uuids {
                    b = b.owner_uuids(v);
                }
                if let Some(v) = group {
                    b = b.group(v);
                }
                if let Some(v) = description {
                    b = b.description(v);
                }
                if let Some(v) = common_name {
                    b = b.common_name(v);
                }
                if let Some(v) = parent {
                    b = b.parent(v);
                }
                if let Some(v) = fss {
                    b = b.fss(v);
                }
                if let Some(v) = cpu_burst_ratio {
                    b = b.cpu_burst_ratio(v);
                }
                if let Some(v) = ram_ratio {
                    b = b.ram_ratio(v);
                }
                if let Some(v) = billing_tag {
                    b = b.billing_tag(v);
                }
                if let Some(v) = alloc_server_spread {
                    b = b.alloc_server_spread(v);
                }
                if let Some(v) = flexible_disk {
                    b = b.flexible_disk(v);
                }
                if let Some(v) = force {
                    b = b.force(v);
                }
                if let Some(v) = skip_validation {
                    b = b.skip_validation(v);
                }
                b
            });

            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update package failed: {}", e))?;
            let pkg = resp.into_inner();

            if raw {
                println!("{}", serde_json::to_string_pretty(&pkg)?);
            } else {
                print_package_detail(&pkg);
            }
        }

        Commands::Delete { uuid, force } => {
            let mut req = client.delete_package().uuid(uuid);
            if force {
                req = req.force(true);
            }

            req.send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete package failed: {}", e))?;
            println!("Package {} deleted", uuid);
        }
    }

    Ok(())
}

/// Print a package in human-readable detail format.
fn print_package_detail(pkg: &types::Package) {
    println!("UUID:       {}", pkg.uuid);
    println!("Name:       {}", pkg.name);
    println!("Version:    {}", pkg.version);
    println!("Active:     {}", pkg.active);
    if let Some(ref brand) = pkg.brand {
        println!("Brand:      {}", enum_to_display(brand));
    }
    println!("Memory:     {} MiB", pkg.max_physical_memory);
    println!("Swap:       {} MiB", pkg.max_swap);
    println!("Quota:      {} MiB", pkg.quota);
    println!("Max LWPs:   {}", pkg.max_lwps);
    if let Some(cpu_cap) = pkg.cpu_cap {
        println!("CPU Cap:    {}", cpu_cap);
    }
    println!("ZFS IO Pri: {}", pkg.zfs_io_priority);
    if let Some(vcpus) = pkg.vcpus {
        println!("vCPUs:      {}", vcpus);
    }
    if let Some(ref group) = pkg.group {
        println!("Group:      {}", group);
    }
    if let Some(ref desc) = pkg.description {
        println!("Desc:       {}", desc);
    }
    if let Some(ref cn) = pkg.common_name {
        println!("Common:     {}", cn);
    }
    if let Some(ref os) = pkg.os {
        println!("OS:         {}", os);
    }
    if let Some(ref parent) = pkg.parent {
        println!("Parent:     {}", parent);
    }
    if let Some(fss) = pkg.fss {
        println!("FSS:        {}", fss);
    }
    if let Some(ref spread) = pkg.alloc_server_spread {
        println!("Spread:     {}", enum_to_display(spread));
    }
    if let Some(flex) = pkg.flexible_disk {
        println!("Flex Disk:  {}", flex);
    }
    if let Some(ref tag) = pkg.billing_tag {
        println!("Billing:    {}", tag);
    }
    if let Some(ref owners) = pkg.owner_uuids {
        let uuids: Vec<String> = owners.iter().map(|u| u.to_string()).collect();
        println!("Owners:     {}", uuids.join(", "));
    }
    if let Some(ref created) = pkg.created_at {
        println!("Created:    {}", created);
    }
    if let Some(ref updated) = pkg.updated_at {
        println!("Updated:    {}", updated);
    }
}
