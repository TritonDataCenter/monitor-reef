// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tritonadm - Triton datacenter administration CLI (Rust successor to sdcadm)

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

#[derive(Parser)]
#[command(
    name = "tritonadm",
    version,
    about = "Administer a Triton datacenter",
    long_about = "Tool for managing Triton datacenter services, instances, and configuration.\n\
                   This is the Rust successor to the Node.js sdcadm tool."
)]
struct Cli {
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
    Instances,

    /// Rollback Triton services and instances
    Rollback,

    /// Update tritonadm itself
    SelfUpdate,

    /// List all Triton services
    Services,

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

fn main() {
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
        }
        Commands::Create => not_yet_implemented("create"),
        Commands::DefaultFabric => not_yet_implemented("default-fabric"),
        Commands::Instances => not_yet_implemented("instances"),
        Commands::Rollback => not_yet_implemented("rollback"),
        Commands::SelfUpdate => not_yet_implemented("self-update"),
        Commands::Services => not_yet_implemented("services"),
        Commands::Update => not_yet_implemented("update"),
        Commands::Channel { command } => command.run(),
        Commands::DcMaint { command } => command.run(),
        Commands::Platform { command } => command.run(),
        Commands::PostSetup { command } => command.run(),
        Commands::Experimental { command } => command.run(),
    }
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
