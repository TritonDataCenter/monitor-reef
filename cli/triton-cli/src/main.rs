// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Triton CLI - User-friendly command-line interface for Triton CloudAPI

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
mod config;
mod output;

use commands::ProfileCommand;

#[derive(Parser)]
#[command(
    name = "triton",
    version,
    about = "Triton cloud management CLI",
    long_about = "User-friendly command-line interface for Triton CloudAPI"
)]
struct Cli {
    /// Profile to use
    #[arg(short, long, global = true, env = "TRITON_PROFILE")]
    profile: Option<String>,

    /// CloudAPI URL override
    #[arg(short = 'U', long, global = true, env = "TRITON_URL")]
    url: Option<String>,

    /// Account name override
    #[arg(short, long, global = true, env = "TRITON_ACCOUNT")]
    account: Option<String>,

    /// SSH key fingerprint override
    #[arg(short, long, global = true, env = "TRITON_KEY_ID")]
    key_id: Option<String>,

    /// Output as JSON
    #[arg(short, long, global = true)]
    json: bool,

    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage connection profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },

    /// Generate shell environment exports
    Env {
        /// Profile name (defaults to current)
        profile: Option<String>,
        /// Shell type (bash, fish, powershell)
        #[arg(short, long, default_value = "bash")]
        shell: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up logging
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_env_filter("triton=debug")
            .init();
    }

    match cli.command {
        Commands::Profile { command } => command.run().await,
        Commands::Env { profile, shell } => commands::env::generate_env(profile.as_deref(), &shell),
    }
}
