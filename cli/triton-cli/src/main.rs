// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton CLI - User-friendly command-line interface for Triton CloudAPI

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use cloudapi_client::TypedClient;

mod commands;
mod config;
mod output;

use commands::{
    AccountCommand, FwruleCommand, ImageCommand, InstanceCommand, KeyCommand, NetworkCommand,
    PackageCommand, ProfileCommand, RbacCommand, VlanCommand, VolumeCommand,
};
use config::profile::{Config, Profile};

/// Custom version string matching node-triton format
fn version_string() -> &'static str {
    concat!(
        "Triton CLI ",
        env!("CARGO_PKG_VERSION"),
        "\nhttps://github.com/TritonDataCenter/triton-rust-monorepo"
    )
}

#[derive(Parser)]
#[command(
    name = "triton",
    version = version_string(),
    about = "Triton cloud management CLI",
    long_about = "User-friendly command-line interface for Triton CloudAPI"
)]
struct Cli {
    /// Profile to use
    #[arg(short, long, env = "TRITON_PROFILE")]
    profile: Option<String>,

    /// CloudAPI URL override
    #[arg(short = 'U', long, env = "TRITON_URL")]
    url: Option<String>,

    /// Account name override
    #[arg(short, long, env = "TRITON_ACCOUNT")]
    account: Option<String>,

    /// SSH key fingerprint override
    #[arg(short, long, env = "TRITON_KEY_ID")]
    key_id: Option<String>,

    /// Output as JSON
    #[arg(short, long, global = true)]
    json: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,

    /// RBAC sub-user login name
    #[arg(short = 'u', long = "user", env = "TRITON_RBAC_USER")]
    user: Option<String>,

    /// RBAC role(s) to assume (can be repeated)
    #[arg(short = 'r', long = "role", env = "TRITON_RBAC_ROLE")]
    role: Vec<String>,

    /// Skip TLS certificate validation (insecure)
    #[arg(short = 'i', long = "insecure", env = "TRITON_TLS_INSECURE")]
    insecure: bool,

    /// Act as another account (operator only)
    #[arg(long = "act-as")]
    act_as: Option<String>,

    /// CloudAPI version to request
    #[arg(long = "accept-version", hide = true)]
    accept_version: Option<String>,

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

    /// Manage images
    #[command(alias = "img")]
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },

    /// Manage SSH keys
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },

    /// Manage networks
    #[command(alias = "net")]
    Network {
        #[command(subcommand)]
        command: NetworkCommand,
    },

    /// Manage firewall rules
    Fwrule {
        #[command(subcommand)]
        command: FwruleCommand,
    },

    /// Manage fabric VLANs
    Vlan {
        #[command(subcommand)]
        command: VlanCommand,
    },

    /// Manage volumes
    #[command(alias = "vol")]
    Volume {
        #[command(subcommand)]
        command: VolumeCommand,
    },

    /// Manage packages
    #[command(alias = "pkg")]
    Package {
        #[command(subcommand)]
        command: PackageCommand,
    },

    /// Manage account settings
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },

    /// Manage RBAC (users, roles, policies)
    Rbac {
        #[command(subcommand)]
        command: RbacCommand,
    },

    /// Show account info and resource usage
    Info,

    /// List datacenters
    #[command(alias = "dcs")]
    Datacenters,

    /// List service endpoints
    #[command(alias = "svcs")]
    Services,

    /// Subscribe to VM change events
    Changefeed(commands::changefeed::ChangefeedArgs),

    // =========================================================================
    // TOP-LEVEL SHORTCUTS
    // =========================================================================
    /// List instances (shortcut for 'instance list')
    #[command(alias = "instances", alias = "ls")]
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

    /// List images (shortcut for 'image list')
    #[command(alias = "images")]
    Imgs(commands::image::ImageListArgs),

    /// List packages (shortcut for 'package list')
    #[command(alias = "packages")]
    Pkgs(commands::package::PackageListArgs),

    /// List networks (shortcut for 'network list')
    #[command(alias = "networks")]
    Nets(commands::network::NetworkListArgs),

    /// List volumes (shortcut for 'volume list')
    #[command(alias = "volumes")]
    Vols(commands::volume::VolumeListArgs),

    /// List SSH keys (shortcut for 'key list')
    Keys,

    /// List firewall rules (shortcut for 'fwrule list')
    Fwrules,

    /// List VLANs (shortcut for 'vlan list')
    Vlans,

    /// List profiles (shortcut for 'profile list')
    Profiles,

    /// Get instance IP (shortcut for 'instance ip')
    Ip(commands::instance::get::IpArgs),

    /// List instance disks (shortcut for 'instance disk list')
    Disks(commands::instance::disk::DiskListArgs),

    /// List instance snapshots (shortcut for 'instance snapshot list')
    Snapshots(commands::instance::snapshot::SnapshotListArgs),

    /// List instance tags (shortcut for 'instance tag list')
    Tags(commands::instance::tag::TagListArgs),

    /// List instance metadata (shortcut for 'instance metadata list')
    #[command(alias = "metadata")]
    Metadatas(commands::instance::metadata::MetadataListArgs),

    /// List instance NICs (shortcut for 'instance nic list')
    Nics(commands::instance::nic::NicListArgs),

    /// Generate shell completions
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Badger don't care
    #[command(hide = true)]
    Badger,

    /// Make raw authenticated API requests to CloudAPI
    #[command(hide = true)]
    Cloudapi(commands::cloudapi::CloudApiArgs),
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
            let mut auth_config = triton_auth::AuthConfig::new(
                account,
                key_id.clone(),
                triton_auth::KeySource::auto(&key_id),
            );

            // Apply RBAC options from CLI
            if let Some(user) = &self.user {
                auth_config = auth_config.with_user(user.clone());
            }
            if !self.role.is_empty() {
                auth_config = auth_config.with_roles(self.role.clone());
            }
            if let Some(act_as) = &self.act_as {
                auth_config = auth_config.with_act_as(act_as.clone());
            }
            if let Some(version) = &self.accept_version {
                auth_config = auth_config.with_accept_version(version.clone());
            }

            return Ok(TypedClient::new_with_insecure(
                &url,
                auth_config,
                self.insecure,
            )?);
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

        // Apply RBAC options: CLI overrides profile
        if let Some(user) = self.user.as_ref().or(profile.user.as_ref()) {
            auth_config = auth_config.with_user(user.clone());
        }
        if !self.role.is_empty() {
            auth_config = auth_config.with_roles(self.role.clone());
        } else if let Some(roles) = &profile.roles {
            auth_config = auth_config.with_roles(roles.clone());
        }
        if let Some(act_as) = self.act_as.as_ref().or(profile.act_as_account.as_ref()) {
            auth_config = auth_config.with_act_as(act_as.clone());
        }
        if let Some(version) = &self.accept_version {
            auth_config = auth_config.with_accept_version(version.clone());
        }

        // Insecure mode: CLI flag or profile setting
        let insecure = self.insecure || profile.insecure;

        Ok(TypedClient::new_with_insecure(
            &final_url,
            auth_config,
            insecure,
        )?)
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
            command.clone().run(&client, cli.json).await
        }
        Commands::Image { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Key { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Network { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Fwrule { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Vlan { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Volume { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Package { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Account { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Rbac { command } => {
            let client = cli.build_client()?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Info => {
            let client = cli.build_client()?;
            commands::info::run(&client, cli.json).await
        }
        Commands::Datacenters => {
            let client = cli.build_client()?;
            commands::datacenters::run(&client, cli.json).await
        }
        Commands::Services => {
            let client = cli.build_client()?;
            commands::services::run(&client, cli.json).await
        }
        Commands::Changefeed(args) => {
            let client = cli.build_client()?;
            commands::changefeed::run(args.clone(), &client, cli.json).await
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
        Commands::Imgs(args) => {
            let client = cli.build_client()?;
            commands::image::ImageCommand::List(args.clone())
                .run(&client, cli.json)
                .await
        }
        Commands::Pkgs(args) => {
            let client = cli.build_client()?;
            commands::package::PackageCommand::List(args.clone())
                .run(&client, cli.json)
                .await
        }
        Commands::Nets(args) => {
            let client = cli.build_client()?;
            commands::network::NetworkCommand::List(args.clone())
                .run(&client, cli.json)
                .await
        }
        Commands::Vols(args) => {
            let client = cli.build_client()?;
            commands::volume::VolumeCommand::List(args.clone())
                .run(&client, cli.json)
                .await
        }
        Commands::Keys => {
            let client = cli.build_client()?;
            commands::key::KeyCommand::List(commands::key::KeyListArgs {
                table: Default::default(),
                authorized_keys: false,
            })
            .run(&client, cli.json)
            .await
        }
        Commands::Fwrules => {
            let client = cli.build_client()?;
            commands::fwrule::FwruleCommand::List(commands::fwrule::FwruleListArgs {
                table: Default::default(),
            })
            .run(&client, cli.json)
            .await
        }
        Commands::Vlans => {
            let client = cli.build_client()?;
            commands::vlan::VlanCommand::List(commands::vlan::VlanListArgs {
                filters: vec![],
                table: Default::default(),
            })
            .run(&client, cli.json)
            .await
        }
        Commands::Profiles => {
            commands::profile::ProfileCommand::List(commands::profile::ProfileListArgs {
                json: cli.json,
                table: Default::default(),
            })
            .run()
            .await
        }
        Commands::Ip(args) => {
            let client = cli.build_client()?;
            commands::instance::get::ip(args.clone(), &client).await
        }
        Commands::Disks(args) => {
            let client = cli.build_client()?;
            commands::instance::disk::list_disks(args.clone(), &client, cli.json).await
        }
        Commands::Snapshots(args) => {
            let client = cli.build_client()?;
            commands::instance::snapshot::list_snapshots(args.clone(), &client, cli.json).await
        }
        Commands::Tags(args) => {
            let client = cli.build_client()?;
            commands::instance::tag::list_tags(args.clone(), &client, cli.json).await
        }
        Commands::Metadatas(args) => {
            let client = cli.build_client()?;
            commands::instance::metadata::list_metadata(args.clone(), &client, cli.json).await
        }
        Commands::Nics(args) => {
            let client = cli.build_client()?;
            commands::instance::nic::list_nics(args.clone(), &client, cli.json).await
        }
        Commands::Completion { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            generate(*shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
        Commands::Badger => {
            print!("{}", include_str!("../assets/badger"));
            Ok(())
        }
        Commands::Cloudapi(args) => {
            let client = cli.build_client()?;
            commands::cloudapi::run(args.clone(), &client).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Test that the CLI structure is valid and has no conflicts.
    ///
    /// This catches issues like:
    /// - Duplicate short options (e.g., two args using `-n`)
    /// - Duplicate long options
    /// - Invalid argument configurations
    ///
    /// Clap's debug_assert() validates the entire command tree including
    /// all subcommands, so this single test covers the whole CLI.
    #[test]
    fn verify_cli_structure() {
        // This will panic if there are any argument conflicts or invalid configurations
        Cli::command().debug_assert();
    }
}
