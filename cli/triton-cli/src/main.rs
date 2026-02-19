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

mod cache;
mod commands;
mod config;
mod output;

use commands::{
    AccesskeyCommand, AccountCommand, FwruleCommand, ImageCommand, InstanceCommand, KeyCommand,
    NetworkCommand, PackageCommand, ProfileCommand, RbacCommand, VlanCommand, VolumeCommand,
};
use config::profile::Profile;
use config::resolve_profile;

/// Custom version string matching node-triton format
fn version_string() -> &'static str {
    concat!(
        "Triton CLI ",
        env!("CARGO_PKG_VERSION"),
        "\nhttps://github.com/TritonDataCenter/monitor-reef"
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
        #[arg(long, default_value = "bash")]
        shell: String,
        /// Emit only the Triton section
        #[arg(short = 't', long = "triton")]
        triton_section: bool,
        /// Emit only the Docker section
        #[arg(short = 'd', long)]
        docker: bool,
        /// Emit only the SmartDC/SDC section
        #[arg(short = 's', long)]
        smartdc: bool,
        /// Emit unset commands instead of exports
        #[arg(short = 'u', long)]
        unset: bool,
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

    /// Manage access keys
    Accesskey {
        #[command(subcommand)]
        command: AccesskeyCommand,
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

    /// List access keys (shortcut for 'accesskey list')
    #[command(hide = true)]
    Accesskeys(commands::accesskey::AccesskeyListArgs),

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

/// Extra certificate locations to probe on platforms where `openssl-probe`
/// doesn't find the system CA store (e.g., SmartOS/illumos with pkgsrc).
const EXTRA_CERT_FILES: &[&str] = &[
    "/opt/local/etc/openssl/certs/ca-certificates.crt",
    "/etc/ssl/certs/ca-certificates.crt",
];
const EXTRA_CERT_DIRS: &[&str] = &["/opt/local/etc/openssl/certs", "/etc/ssl/certs"];

/// Build a root certificate store with a three-tier fallback:
///
/// 1. Native system certs (via `rustls-native-certs` / `openssl-probe`)
/// 2. Extra platform-specific paths (SmartOS pkgsrc, etc.)
/// 3. Bundled Mozilla roots (via `webpki-roots`) as a last resort
///
/// This handles platforms like SmartOS/illumos where `openssl-probe` doesn't
/// check the paths where certificates are actually installed.
async fn build_root_cert_store() -> rustls::RootCertStore {
    let mut root_store = rustls::RootCertStore::empty();

    // 1. Try native certs (respects SSL_CERT_FILE / SSL_CERT_DIR)
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = root_store.add(cert);
    }
    if !root_store.is_empty() {
        return root_store;
    }

    // 2. Probe extra platform-specific paths
    load_extra_cert_paths(&mut root_store).await;
    if !root_store.is_empty() {
        return root_store;
    }

    // 3. Fall back to bundled Mozilla roots
    eprintln!(
        "\
warning: no native root certificates found; using bundled Mozilla roots

  If you need to trust additional CAs (e.g., a self-signed certificate),
  point the TLS library at your certificate store:

    export SSL_CERT_FILE=/path/to/ca-bundle.pem
    export SSL_CERT_DIR=/path/to/certs/directory"
    );
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    root_store
}

/// Try loading PEM certificates from extra platform-specific paths into the
/// root store. Stops as soon as any certificates are loaded.
async fn load_extra_cert_paths(root_store: &mut rustls::RootCertStore) {
    // Try bundle files first (single file containing many PEM certs)
    for path in EXTRA_CERT_FILES {
        if let Ok(data) = tokio::fs::read(path).await {
            let mut cursor = std::io::Cursor::new(data);
            for cert in rustls_pemfile::certs(&mut cursor).flatten() {
                let _ = root_store.add(cert);
            }
            if !root_store.is_empty() {
                return;
            }
        }
    }

    // Try cert directories (individual PEM files, including OpenSSL hash symlinks)
    for dir_path in EXTRA_CERT_DIRS {
        let Ok(mut entries) = tokio::fs::read_dir(dir_path).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Ok(data) = tokio::fs::read(&path).await {
                let mut cursor = std::io::Cursor::new(data);
                for cert in rustls_pemfile::certs(&mut cursor).flatten() {
                    let _ = root_store.add(cert);
                }
            }
        }
        if !root_store.is_empty() {
            return;
        }
    }
}

/// Build a reqwest HTTP client with CA cert fallback for platforms where
/// the default certificate store isn't found (e.g., SmartOS/illumos).
async fn build_http_client(insecure: bool) -> Result<reqwest::Client> {
    let root_store = build_root_cert_store().await;
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    reqwest::Client::builder()
        .danger_accept_invalid_certs(insecure)
        .use_preconfigured_tls(tls_config)
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))
}

impl Cli {
    /// Build an authenticated TypedClient from CLI options or profile
    ///
    /// Uses `resolve_profile` as the single source of truth for profile
    /// resolution, then applies CLI/env overrides on top.
    async fn build_client(&self) -> Result<(TypedClient, Profile)> {
        let profile = resolve_profile(self.profile.as_deref()).await?;

        // Allow CLI/env overrides on top of the resolved profile.
        // self.url/account/key_id pick up TRITON_* vars via clap's `env`,
        // and we also check SDC_* as a legacy fallback.
        let final_url = self
            .url
            .clone()
            .or_else(|| std::env::var("SDC_URL").ok())
            .unwrap_or_else(|| profile.url.clone());
        let final_account = self
            .account
            .clone()
            .or_else(|| std::env::var("SDC_ACCOUNT").ok())
            .unwrap_or_else(|| profile.account.clone());
        let final_key_id = self
            .key_id
            .clone()
            .or_else(|| std::env::var("SDC_KEY_ID").ok())
            .unwrap_or_else(|| profile.key_id.clone());

        let mut auth_config = triton_auth::AuthConfig::new(
            final_account,
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

        let http_client = build_http_client(insecure).await?;
        Ok((
            TypedClient::new_with_http_client(&final_url, auth_config, http_client),
            profile,
        ))
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = try_main().await {
        // Format with alternate display to include the full error chain.
        let msg = format!("{e:#}");
        // Progenitor's Error::Custom Display prepends "Error: ", which
        // duplicates the prefix we add here. Strip it to avoid
        // "triton: error: Error: ...".
        let msg = msg.strip_prefix("Error: ").unwrap_or(&msg);
        eprintln!("triton: error: {msg}");
        std::process::exit(1);
    }
}

async fn try_main() -> Result<()> {
    let cli = Cli::parse();

    // Warn if profiles exist in an alternative config directory
    config::paths::warn_alternative_config_dirs().await;

    // Set up logging
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_env_filter("triton=debug")
            .init();
    }

    match &cli.command {
        Commands::Profile { command } => command.clone().run().await,
        Commands::Env {
            profile,
            shell,
            triton_section,
            docker,
            smartdc,
            unset,
        } => {
            commands::env::generate_env(
                profile.as_deref(),
                shell,
                *triton_section,
                *docker,
                *smartdc,
                *unset,
            )
            .await
        }
        Commands::Instance { command } if command.is_empty_variadic() => Ok(()),
        Commands::Instance { command } => {
            let (client, profile) = cli.build_client().await?;
            let cache = cache::ImageCache::new(&profile).await;
            command.clone().run(&client, cli.json, cache.as_ref()).await
        }
        Commands::Image { command } => {
            let (client, profile) = cli.build_client().await?;
            let cache = cache::ImageCache::new(&profile).await;
            command.clone().run(&client, cli.json, cache.as_ref()).await
        }
        Commands::Key { command } if command.is_empty_variadic() => Ok(()),
        Commands::Key { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Accesskey { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Network { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Fwrule { command } if command.is_empty_variadic() => Ok(()),
        Commands::Fwrule { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Vlan { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Volume { command } if command.is_empty_variadic() => Ok(()),
        Commands::Volume { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Package { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Account { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Rbac { command } => {
            let (client, _profile) = cli.build_client().await?;
            command.clone().run(&client, cli.json).await
        }
        Commands::Info => {
            let (client, _profile) = cli.build_client().await?;
            commands::info::run(&client, cli.json).await
        }
        Commands::Datacenters => {
            let (client, _profile) = cli.build_client().await?;
            commands::datacenters::run(&client, cli.json).await
        }
        Commands::Services => {
            let (client, _profile) = cli.build_client().await?;
            commands::services::run(&client, cli.json).await
        }
        Commands::Changefeed(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::changefeed::run(args.clone(), &client, cli.json).await
        }
        Commands::Insts(args) => {
            let (client, profile) = cli.build_client().await?;
            let cache = cache::ImageCache::new(&profile).await;
            commands::instance::list::run(args.clone(), &client, cli.json, cache.as_ref()).await
        }
        Commands::Create(args) => {
            let (client, profile) = cli.build_client().await?;
            let cache = cache::ImageCache::new(&profile).await;
            commands::instance::create::run(args.clone(), &client, cli.json, cache.as_ref()).await
        }
        Commands::Ssh(args) => {
            let (client, profile) = cli.build_client().await?;
            let cache = cache::ImageCache::new(&profile).await;
            commands::instance::ssh::run(args.clone(), &client, cache.as_ref()).await
        }
        Commands::Start(args) if args.instances.is_empty() => Ok(()),
        Commands::Start(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::instance::lifecycle::start(args.clone(), &client).await
        }
        Commands::Stop(args) if args.instances.is_empty() => Ok(()),
        Commands::Stop(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::instance::lifecycle::stop(args.clone(), &client).await
        }
        Commands::Reboot(args) if args.instances.is_empty() => Ok(()),
        Commands::Reboot(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::instance::lifecycle::reboot(args.clone(), &client).await
        }
        Commands::Delete(args) if args.instances.is_empty() => Ok(()),
        Commands::Delete(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::instance::delete::run(args.clone(), &client).await
        }
        Commands::Imgs(args) => {
            let (client, profile) = cli.build_client().await?;
            let cache = cache::ImageCache::new(&profile).await;
            commands::image::ImageCommand::List(args.clone())
                .run(&client, cli.json, cache.as_ref())
                .await
        }
        Commands::Pkgs(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::package::PackageCommand::List(args.clone())
                .run(&client, cli.json)
                .await
        }
        Commands::Nets(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::network::NetworkCommand::List(args.clone())
                .run(&client, cli.json)
                .await
        }
        Commands::Vols(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::volume::VolumeCommand::List(args.clone())
                .run(&client, cli.json)
                .await
        }
        Commands::Accesskeys(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::accesskey::AccesskeyCommand::List(args.clone())
                .run(&client, cli.json)
                .await
        }
        Commands::Keys => {
            let (client, _profile) = cli.build_client().await?;
            commands::key::KeyCommand::List(commands::key::KeyListArgs {
                table: Default::default(),
                authorized_keys: false,
            })
            .run(&client, cli.json)
            .await
        }
        Commands::Fwrules => {
            let (client, _profile) = cli.build_client().await?;
            commands::fwrule::FwruleCommand::List(commands::fwrule::FwruleListArgs {
                table: Default::default(),
            })
            .run(&client, cli.json)
            .await
        }
        Commands::Vlans => {
            let (client, _profile) = cli.build_client().await?;
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
            let (client, _profile) = cli.build_client().await?;
            commands::instance::get::ip(args.clone(), &client).await
        }
        Commands::Disks(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::instance::disk::list_disks(args.clone(), &client, cli.json).await
        }
        Commands::Snapshots(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::instance::snapshot::list_snapshots(args.clone(), &client, cli.json).await
        }
        Commands::Tags(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::instance::tag::list_tags(args.clone(), &client, cli.json).await
        }
        Commands::Metadatas(args) => {
            let (client, _profile) = cli.build_client().await?;
            commands::instance::metadata::list_metadata(args.clone(), &client, cli.json).await
        }
        Commands::Nics(args) => {
            let (client, _profile) = cli.build_client().await?;
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
            let (client, _profile) = cli.build_client().await?;
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
