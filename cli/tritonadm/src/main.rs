// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tritonadm - Triton datacenter administration CLI (Rust successor to sdcadm)

use std::collections::HashMap;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};

mod commands;

use commands::{
    ChannelCommand, DcMaintCommand, ExperimentalCommand, PlatformCommand, PostSetupCommand,
};

/// Print a "not yet implemented" message and exit with code 1.
fn not_yet_implemented(command: &str) -> ! {
    eprintln!("tritonadm {command}: not yet implemented");
    std::process::exit(1);
}

/// Convert a serde-serializable enum value to its wire-format string.
fn enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", val))
}

#[derive(Parser)]
#[command(
    name = "tritonadm",
    version,
    about = "Administer a Triton datacenter",
    long_about = "Tool for managing Triton datacenter services, instances, and configuration.\n\
                   This is the Rust successor to the Node.js sdcadm tool."
)]
struct Cli {
    /// SAPI base URL
    #[arg(
        long,
        env = "SAPI_URL",
        default_value = "http://localhost",
        global = true
    )]
    sapi_url: String,

    /// VMAPI base URL
    #[arg(
        long,
        env = "VMAPI_URL",
        default_value = "http://localhost",
        global = true
    )]
    vmapi_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Display images available for update of Triton services and instances
    Avail,

    /// Check Triton config in SAPI versus system reality
    CheckConfig,

    /// Check that services or instances are up
    CheckHealth,

    /// Output shell completion code
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Create one or more instances of an existing Triton VM service
    Create,

    /// Initialize a default fabric for an account
    DefaultFabric,

    /// List all Triton service instances
    #[command(alias = "insts")]
    Instances {
        /// Output full JSON instead of table
        #[arg(long, short)]
        json: bool,
    },

    /// Rollback Triton services and instances
    Rollback,

    /// Update tritonadm itself
    SelfUpdate,

    /// List all Triton services
    #[command(alias = "svcs")]
    Services {
        /// Output full JSON instead of table
        #[arg(long, short)]
        json: bool,
    },

    /// Update Triton services and instances
    Update,

    /// Manage update channels
    Channel {
        #[command(subcommand)]
        command: ChannelCommand,
    },

    /// Manage datacenter maintenance windows
    DcMaint {
        #[command(subcommand)]
        command: DcMaintCommand,
    },

    /// Manage platforms (OS images for compute nodes)
    Platform {
        #[command(subcommand)]
        command: PlatformCommand,
    },

    /// Post-setup steps for configuring Triton components
    PostSetup {
        #[command(subcommand)]
        command: PostSetupCommand,
    },

    /// Experimental and less-stable commands
    Experimental {
        #[command(subcommand)]
        command: ExperimentalCommand,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Avail => not_yet_implemented("avail"),
        Commands::CheckConfig => not_yet_implemented("check-config"),
        Commands::CheckHealth => not_yet_implemented("check-health"),
        Commands::Completion { shell } => {
            let mut cmd = Cli::command();
            let name = std::env::args()
                .next()
                .and_then(|s| {
                    std::path::Path::new(&s)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| cmd.get_name().to_string());
            generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
        Commands::Create => not_yet_implemented("create"),
        Commands::DefaultFabric => not_yet_implemented("default-fabric"),
        Commands::Instances { json } => cmd_instances(&cli.sapi_url, &cli.vmapi_url, json).await,
        Commands::Rollback => not_yet_implemented("rollback"),
        Commands::SelfUpdate => not_yet_implemented("self-update"),
        Commands::Services { json } => cmd_services(&cli.sapi_url, json).await,
        Commands::Update => not_yet_implemented("update"),
        Commands::Channel { command } => command.run(),
        Commands::DcMaint { command } => command.run(),
        Commands::Platform { command } => command.run(),
        Commands::PostSetup { command } => command.run(),
        Commands::Experimental { command } => command.run(),
    }
}

/// Fetch instance count per service from SAPI.
async fn get_instance_counts(
    sapi: &sapi_client::Client,
) -> Result<HashMap<sapi_client::Uuid, usize>> {
    let instances = sapi
        .list_instances()
        .send()
        .await
        .context("failed to list instances")?
        .into_inner();
    let mut counts: HashMap<sapi_client::Uuid, usize> = HashMap::new();
    for inst in &instances {
        *counts.entry(inst.service_uuid).or_default() += 1;
    }
    Ok(counts)
}

async fn cmd_services(sapi_url: &str, json: bool) -> Result<()> {
    let sapi = sapi_client::Client::new(sapi_url);
    let services = sapi
        .list_services()
        .send()
        .await
        .context("failed to list services")?
        .into_inner();

    if json {
        println!("{}", serde_json::to_string_pretty(&services)?);
    } else {
        // Match sdcadm default columns: TYPE UUID NAME IMAGE INSTS
        let counts = get_instance_counts(&sapi).await?;
        println!(
            "{:<8} {:<38} {:<24} {:<38} INSTS",
            "TYPE", "UUID", "NAME", "IMAGE"
        );
        for svc in &services {
            let type_str = svc.type_.as_ref().map(enum_to_display).unwrap_or_default();
            let image = svc
                .params
                .as_ref()
                .and_then(|p| p.get("image_uuid"))
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let insts = counts.get(&svc.uuid).copied().unwrap_or(0);
            println!(
                "{:<8} {:<38} {:<24} {:<38} {}",
                type_str, svc.uuid, svc.name, image, insts
            );
        }
    }
    Ok(())
}

async fn cmd_instances(sapi_url: &str, vmapi_url: &str, json: bool) -> Result<()> {
    let sapi = sapi_client::Client::new(sapi_url);
    let vmapi = vmapi_client::Client::new(vmapi_url);

    // Fetch instances and services from SAPI
    let instances = sapi
        .list_instances()
        .send()
        .await
        .context("failed to list instances")?
        .into_inner();
    let services = sapi
        .list_services()
        .send()
        .await
        .context("failed to list services")?
        .into_inner();

    let svc_name: HashMap<_, _> = services.iter().map(|s| (s.uuid, s.name.as_str())).collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&instances)?);
    } else {
        // Enrich VM instances with alias and state from VMAPI
        // Matches sdcadm: INSTANCE SERVICE ALIAS STATE IMAGE
        println!(
            "{:<38} {:<20} {:<28} {:<12} IMAGE",
            "INSTANCE", "SERVICE", "ALIAS", "STATE"
        );
        for inst in &instances {
            let service_name = svc_name
                .get(&inst.service_uuid)
                .copied()
                .unwrap_or("unknown");

            // Try to get VM details from VMAPI for enrichment
            let (alias, state, image) = match vmapi.get_vm().uuid(inst.uuid).send().await {
                Ok(resp) => {
                    let vm = resp.into_inner();
                    (
                        vm.alias.unwrap_or_default(),
                        enum_to_display(&vm.state),
                        vm.image_uuid
                            .map(|u| u.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                    )
                }
                Err(_) => ("-".to_string(), "-".to_string(), "-".to_string()),
            };
            println!(
                "{:<38} {:<20} {:<28} {:<12} {}",
                inst.uuid, service_name, alias, state, image
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Test that the CLI structure is valid and has no conflicts.
    #[test]
    fn verify_cli_structure() {
        Cli::command().debug_assert();
    }
}
