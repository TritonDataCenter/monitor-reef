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
mod errors;
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
        " (",
        env!("GIT_COMMIT_SHORT"),
        env!("GIT_DIRTY_SUFFIX"),
        ")",
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

    /// Print version information
    Version,

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
    let mut loaded = 0u32;
    let mut skipped = 0u32;
    for cert in rustls_native_certs::load_native_certs().certs {
        if root_store.add(cert).is_ok() {
            loaded += 1;
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        tracing::debug!(
            "Loaded {} native root certs, skipped {} invalid",
            loaded,
            skipped
        );
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
    tracing::warn!(
        "no native root certificates found; using bundled Mozilla roots\n\n  \
         If you need to trust additional CAs (e.g., a self-signed certificate),\n  \
         point the TLS library at your certificate store:\n\n    \
         export SSL_CERT_FILE=/path/to/ca-bundle.pem\n    \
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
            let mut loaded = 0u32;
            let mut skipped = 0u32;
            for cert in rustls_pemfile::certs(&mut cursor).flatten() {
                if root_store.add(cert).is_ok() {
                    loaded += 1;
                } else {
                    skipped += 1;
                }
            }
            if skipped > 0 {
                tracing::debug!(
                    "Loaded {} root certs, skipped {} invalid from {}",
                    loaded,
                    skipped,
                    path,
                );
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
        let mut loaded = 0u32;
        let mut skipped = 0u32;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Ok(data) = tokio::fs::read(&path).await {
                let mut cursor = std::io::Cursor::new(data);
                for cert in rustls_pemfile::certs(&mut cursor).flatten() {
                    if root_store.add(cert).is_ok() {
                        loaded += 1;
                    } else {
                        skipped += 1;
                    }
                }
            }
        }
        if skipped > 0 {
            tracing::debug!(
                "Loaded {} root certs, skipped {} invalid from {}",
                loaded,
                skipped,
                dir_path,
            );
        }
        if !root_store.is_empty() {
            return;
        }
    }
}

/// Build a reqwest HTTP client with CA cert fallback for platforms where
/// the default certificate store isn't found (e.g., SmartOS/illumos).
async fn build_http_client(insecure: bool) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().danger_accept_invalid_certs(insecure);

    // Only apply custom root cert store when we actually need to verify
    // certificates. When insecure=true, reqwest's built-in handling of
    // danger_accept_invalid_certs is sufficient — adding a preconfigured
    // TLS config would override it and re-enable chain validation.
    if !insecure {
        let root_store = build_root_cert_store().await;
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        builder = builder.use_preconfigured_tls(tls_config);
    }

    builder
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
        // Exit code 3 for resource-not-found (matches Node.js triton convention).
        let exit_code = if e.downcast_ref::<errors::ResourceNotFoundError>().is_some() {
            3
        } else {
            1
        };
        eprintln!("triton: error: {msg}");
        std::process::exit(exit_code);
    }
}

async fn try_main() -> Result<()> {
    let cli = Cli::parse();

    // Set up logging: always show warnings/errors, verbose adds debug
    let filter = if cli.verbose {
        "triton=debug"
    } else {
        "triton=warn"
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .with_target(false)
        .init();

    // Warn if profiles exist in an alternative config directory
    config::paths::warn_alternative_config_dirs().await;

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
            let name = std::env::args()
                .next()
                .and_then(|s| {
                    std::path::Path::new(&s)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| cmd.get_name().to_string());
            generate(*shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
        Commands::Version => {
            println!("{}", version_string());
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

    /// Regression test: build_http_client(insecure=true) must accept
    /// self-signed certificates.
    ///
    /// The custom TLS config (use_preconfigured_tls) previously overrode
    /// reqwest's danger_accept_invalid_certs handling, causing connections
    /// to fail even when insecure=true. Fixed in 1d0349f.
    #[tokio::test]
    async fn insecure_mode_accepts_self_signed_cert() {
        use std::sync::Arc;
        use tokio::net::TcpListener;
        use tokio_rustls::TlsAcceptor;

        // Generate a self-signed certificate for localhost
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("cert generation failed");
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert);
        let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der())
            .expect("key conversion failed");

        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .expect("server config failed");

        let acceptor = TlsAcceptor::from(Arc::new(server_config));

        // Bind to a random port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("https://localhost:{port}/");

        // Spawn a minimal HTTPS server that returns "ok" for each connection
        let acceptor_clone = acceptor.clone();
        let server = tokio::spawn(async move {
            // Accept up to 2 connections (one for insecure=true, one for insecure=false)
            for _ in 0..2 {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let acc = acceptor_clone.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acc.accept(stream).await else {
                        return;
                    };
                    // Read the HTTP request (we don't care about the content)
                    let mut buf = vec![0u8; 4096];
                    let _ = tokio::io::AsyncReadExt::read(&mut tls, &mut buf).await;
                    // Write a minimal HTTP response
                    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
                    let _ = tokio::io::AsyncWriteExt::write_all(&mut tls, response).await;
                });
            }
        });

        // insecure=true must succeed against a self-signed cert
        let client = build_http_client(true)
            .await
            .expect("build insecure client");
        let resp = client.get(&url).send().await;
        assert!(resp.is_ok(), "insecure=true should accept self-signed cert");

        // insecure=false must fail (cert is not in any trust store)
        let client = build_http_client(false).await.expect("build secure client");
        let resp = client.get(&url).send().await;
        assert!(
            resp.is_err(),
            "insecure=false should reject self-signed cert"
        );

        server.abort();
    }
}
