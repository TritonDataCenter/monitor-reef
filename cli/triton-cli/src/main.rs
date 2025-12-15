// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Triton CLI - User-friendly command-line interface for Triton CloudAPI

use anyhow::Result;
use clap::{Parser, Subcommand};
use cloudapi_client::TypedClient;

mod commands;
mod config;
mod output;

use commands::{InstanceCommand, ProfileCommand};
use config::profile::{Config, Profile};

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

    /// Manage instances
    #[command(alias = "inst")]
    Instance {
        #[command(subcommand)]
        command: InstanceCommand,
    },

    /// List instances (shortcut for 'instance list')
    #[command(alias = "instances")]
    Insts(commands::instance::ListArgs),

    /// Create an instance (shortcut for 'instance create')
    Create(commands::instance::create::CreateArgs),

    /// SSH to an instance (shortcut for 'instance ssh')
    Ssh(commands::instance::ssh::SshArgs),

    /// Start instance(s) (shortcut for 'instance start')
    Start(commands::instance::lifecycle::StartArgs),

    /// Stop instance(s) (shortcut for 'instance stop')
    Stop(commands::instance::lifecycle::StopArgs),

    /// Reboot instance(s) (shortcut for 'instance reboot')
    Reboot(commands::instance::lifecycle::RebootArgs),

    /// Delete instance(s) (shortcut for 'instance delete')
    #[command(alias = "rm")]
    Delete(commands::instance::delete::DeleteArgs),
}

impl Cli {
    /// Build an authenticated TypedClient from CLI options or profile
    fn build_client(&self) -> Result<TypedClient> {
        // First try environment variables / CLI overrides
        let url = self.url.clone().or_else(|| std::env::var("SDC_URL").ok());
        let account = self
            .account
            .clone()
            .or_else(|| std::env::var("SDC_ACCOUNT").ok());
        let key_id = self
            .key_id
            .clone()
            .or_else(|| std::env::var("SDC_KEY_ID").ok());

        // If we have all required values from env/CLI, use them directly
        if let (Some(url), Some(account), Some(key_id)) =
            (url.clone(), account.clone(), key_id.clone())
        {
            let auth_config = triton_auth::AuthConfig::new(
                account,
                key_id.clone(),
                triton_auth::KeySource::auto(&key_id),
            );
            return Ok(TypedClient::new(&url, auth_config));
        }

        // Otherwise, load from profile
        let profile_name = self
            .profile
            .clone()
            .or_else(|| Config::load().ok().and_then(|c| c.profile));

        let profile_name = profile_name.ok_or_else(|| {
            anyhow::anyhow!(
                "No profile configured. Use 'triton profile create' or set TRITON_URL, TRITON_ACCOUNT, and TRITON_KEY_ID"
            )
        })?;

        let profile = Profile::load(&profile_name)?;

        // Allow CLI/env overrides on top of profile
        let final_url = url.unwrap_or_else(|| profile.url.clone());
        let final_account = account.unwrap_or_else(|| profile.account.clone());
        let final_key_id = key_id.unwrap_or_else(|| profile.key_id.clone());

        let mut auth_config = triton_auth::AuthConfig::new(
            final_account,
            final_key_id.clone(),
            triton_auth::KeySource::auto(&final_key_id),
        );

        if let Some(user) = &profile.user {
            auth_config = auth_config.with_user(user.clone());
        }
        if let Some(roles) = &profile.roles {
            auth_config = auth_config.with_roles(roles.clone());
        }

        Ok(TypedClient::new(&final_url, auth_config))
    }
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

    match &cli.command {
        Commands::Profile { command } => command.clone().run().await,
        Commands::Env { profile, shell } => commands::env::generate_env(profile.as_deref(), shell),
        Commands::Instance { command } => {
            let client = cli.build_client()?;
            // We need to clone the command since we're borrowing cli
            // This is a bit awkward but necessary with the current structure
            command.clone().run(&client, cli.json).await
        }
        Commands::Insts(args) => {
            let client = cli.build_client()?;
            commands::instance::list::run(args.clone(), &client, cli.json).await
        }
        Commands::Create(args) => {
            let client = cli.build_client()?;
            commands::instance::create::run(args.clone(), &client, cli.json).await
        }
        Commands::Ssh(args) => {
            let client = cli.build_client()?;
            commands::instance::ssh::run(args.clone(), &client).await
        }
        Commands::Start(args) => {
            let client = cli.build_client()?;
            commands::instance::lifecycle::start(args.clone(), &client).await
        }
        Commands::Stop(args) => {
            let client = cli.build_client()?;
            commands::instance::lifecycle::stop(args.clone(), &client).await
        }
        Commands::Reboot(args) => {
            let client = cli.build_client()?;
            commands::instance::lifecycle::reboot(args.clone(), &client).await
        }
        Commands::Delete(args) => {
            let client = cli.build_client()?;
            commands::instance::delete::run(args.clone(), &client).await
        }
    }
}
