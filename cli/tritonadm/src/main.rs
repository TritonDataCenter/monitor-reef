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
mod config;

use commands::{
    ChannelCommand, DcMaintCommand, ExperimentalCommand, ImageCommand, MahiCommand,
    PlatformCommand, PostSetupCommand, SapiCommand, errors::is_404,
};
use config::TritonConfig;

/// Default updates server URL (used when --updates-url / UPDATES_URL not set).
pub const DEFAULT_UPDATES_URL: &str = "https://updates.tritondatacenter.com";

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
    /// SAPI base URL (auto-detected from SDC config if not set)
    #[arg(long, env = "SAPI_URL", global = true)]
    sapi_url: Option<String>,

    /// IMGAPI base URL (auto-detected from SDC config if not set)
    #[arg(long, env = "IMGAPI_URL", global = true)]
    imgapi_url: Option<String>,

    /// VMAPI base URL (auto-detected from SDC config if not set)
    #[arg(long, env = "VMAPI_URL", global = true)]
    vmapi_url: Option<String>,

    /// PAPI base URL (auto-detected from SDC config if not set)
    #[arg(long, env = "PAPI_URL", global = true)]
    papi_url: Option<String>,

    /// NAPI base URL (auto-detected from SDC config if not set)
    #[arg(long, env = "NAPI_URL", global = true)]
    napi_url: Option<String>,

    /// Mahi base URL (auto-detected from SDC config if not set)
    #[arg(long, env = "MAHI_URL", global = true)]
    mahi_url: Option<String>,

    /// Mahi sitter base URL (no SDC default — the sitter has no DNS record)
    #[arg(long, env = "MAHI_SITTER_URL", global = true)]
    mahi_sitter_url: Option<String>,

    /// Updates server URL (default: https://updates.tritondatacenter.com)
    #[arg(long, env = "UPDATES_URL", global = true)]
    updates_url: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

impl Cli {
    /// Resolve the SAPI URL from CLI flag, env var, or SDC config.
    fn sapi_url(&self, sdc_config: &Option<TritonConfig>) -> Result<String> {
        if let Some(url) = &self.sapi_url {
            return Ok(url.clone());
        }
        if let Some(cfg) = sdc_config {
            return Ok(cfg.service_url("sapi"));
        }
        anyhow::bail!(
            "cannot determine SAPI URL: set --sapi-url, SAPI_URL, \
             or run on a Triton headnode"
        )
    }

    /// Resolve the IMGAPI URL from CLI flag, env var, or SDC config.
    fn imgapi_url(&self, sdc_config: &Option<TritonConfig>) -> Result<String> {
        if let Some(url) = &self.imgapi_url {
            return Ok(url.clone());
        }
        if let Some(cfg) = sdc_config {
            return Ok(cfg.service_url("imgapi"));
        }
        anyhow::bail!(
            "cannot determine IMGAPI URL: set --imgapi-url, IMGAPI_URL, \
             or run on a Triton headnode"
        )
    }

    /// Resolve the VMAPI URL from CLI flag, env var, or SDC config.
    fn vmapi_url(&self, sdc_config: &Option<TritonConfig>) -> Result<String> {
        if let Some(url) = &self.vmapi_url {
            return Ok(url.clone());
        }
        if let Some(cfg) = sdc_config {
            return Ok(cfg.service_url("vmapi"));
        }
        anyhow::bail!(
            "cannot determine VMAPI URL: set --vmapi-url, VMAPI_URL, \
             or run on a Triton headnode"
        )
    }

    /// Resolve the PAPI URL from CLI flag, env var, or SDC config.
    fn papi_url(&self, sdc_config: &Option<TritonConfig>) -> Result<String> {
        if let Some(url) = &self.papi_url {
            return Ok(url.clone());
        }
        if let Some(cfg) = sdc_config {
            return Ok(cfg.service_url("papi"));
        }
        anyhow::bail!(
            "cannot determine PAPI URL: set --papi-url, PAPI_URL, \
             or run on a Triton headnode"
        )
    }

    /// Resolve the NAPI URL from CLI flag, env var, or SDC config.
    fn napi_url(&self, sdc_config: &Option<TritonConfig>) -> Result<String> {
        if let Some(url) = &self.napi_url {
            return Ok(url.clone());
        }
        if let Some(cfg) = sdc_config {
            return Ok(cfg.service_url("napi"));
        }
        anyhow::bail!(
            "cannot determine NAPI URL: set --napi-url, NAPI_URL, \
             or run on a Triton headnode"
        )
    }

    /// Resolve the Mahi URL from CLI flag, env var, or SDC config.
    fn mahi_url(&self, sdc_config: &Option<TritonConfig>) -> Result<String> {
        if let Some(url) = &self.mahi_url {
            return Ok(url.clone());
        }
        if let Some(cfg) = sdc_config {
            return Ok(cfg.service_url("mahi"));
        }
        anyhow::bail!(
            "cannot determine Mahi URL: set --mahi-url, MAHI_URL, \
             or run on a Triton headnode"
        )
    }

    /// Resolve the Mahi sitter URL. The sitter runs on port 8080 of the mahi
    /// zone (see `replicator.port` in the mahi SAPI manifest); the main mahi
    /// service is on port 80 of the same zone. Auto-derive from the SDC
    /// config by appending `:8080` to the mahi service URL.
    fn mahi_sitter_url(&self, sdc_config: &Option<TritonConfig>) -> Result<String> {
        if let Some(url) = &self.mahi_sitter_url {
            return Ok(url.clone());
        }
        if let Some(cfg) = sdc_config {
            return Ok(format!("{}:8080", cfg.service_url("mahi")));
        }
        anyhow::bail!(
            "cannot determine Mahi sitter URL: set --mahi-sitter-url, \
             MAHI_SITTER_URL, or run on a Triton headnode"
        )
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Display images available for update of Triton services and instances
    #[command(alias = "available")]
    Avail {
        /// Output full JSON instead of table
        #[arg(long, short)]
        json: bool,
    },

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

    /// Update tritonadm itself (mirrors `sdcadm experimental get-tritonadm`).
    SelfUpdate {
        /// Install the latest image from the update channel. Mutually
        /// exclusive with passing an image UUID.
        #[arg(long)]
        latest: bool,

        /// Pin to a specific image UUID.
        image_uuid: Option<String>,

        /// Update channel to pull from. If omitted, falls back to the
        /// sdc SAPI application's `update_channel` metadata, then to
        /// the updates server's default channel (same fallback chain
        /// as sdcadm's getDefaultChannel).
        #[arg(short = 'C', long)]
        channel: Option<String>,

        /// Run the installer shar with TRACE=1 set so it emits the
        /// full xtrace (PS4-formatted). Matches the flag sdcadm's
        /// get-tritonadm sets unconditionally; we default to off
        /// because self-update runs interactively.
        #[arg(long)]
        verbose: bool,

        /// Resolve everything and print what would happen, but skip
        /// the download and installer exec. Mirrors sdcadm's
        /// self-update -n / get-tritonadm --dry-run.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },

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

    /// Manage images in IMGAPI
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },

    /// Development helpers (not part of sdcadm)
    Dev {
        #[command(subcommand)]
        command: commands::DevCommand,
    },

    /// Raw access to the SAPI HTTP API (applications, services, instances, manifests, ...)
    Sapi {
        #[command(subcommand)]
        command: SapiCommand,
    },

    /// Raw access to the Mahi auth-cache HTTP API (lookup, SigV4, STS, IAM, sitter)
    Mahi {
        #[command(subcommand)]
        command: MahiCommand,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load SDC config from headnode (best-effort — None on non-Triton systems)
    let sdc_config = TritonConfig::load();

    // Resolve API URLs eagerly (lazy resolution per-command would hit
    // borrow-checker issues when match arms destructure cli.command).
    let sapi_url = cli.sapi_url(&sdc_config);
    let imgapi_url = cli.imgapi_url(&sdc_config);
    let vmapi_url = cli.vmapi_url(&sdc_config);
    let papi_url = cli.papi_url(&sdc_config);
    let napi_url = cli.napi_url(&sdc_config);
    let mahi_url = cli.mahi_url(&sdc_config);
    let mahi_sitter_url = cli.mahi_sitter_url(&sdc_config);
    let updates_url = cli.updates_url;

    match cli.command {
        Commands::Avail { json } => cmd_avail(&sapi_url?, &imgapi_url?, json).await,
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
        Commands::Instances { json } => cmd_instances(&sapi_url?, &vmapi_url?, json).await,
        Commands::Rollback => not_yet_implemented("rollback"),
        Commands::SelfUpdate {
            latest,
            image_uuid,
            channel,
            verbose,
            dry_run,
        } => {
            let image_uuid = match (latest, image_uuid) {
                (true, None) => None,
                (false, Some(s)) => Some(
                    uuid::Uuid::parse_str(&s)
                        .context("image uuid argument must be a valid UUID")?,
                ),
                (true, Some(_)) => anyhow::bail!("pass either --latest or an image UUID, not both"),
                (false, None) => anyhow::bail!(
                    "pass --latest or an image UUID (sdcadm experimental \
                     get-tritonadm --latest behaves the same way)"
                ),
            };
            commands::self_update::run(commands::self_update::SelfUpdateOpts {
                updates_url: updates_url
                    .clone()
                    .unwrap_or_else(|| DEFAULT_UPDATES_URL.to_string()),
                // sapi_url is best-effort: if we can auto-detect it
                // (headnode) or it was passed explicitly, great; if
                // not, self_update::run requires --channel.
                sapi_url: sapi_url.as_ref().ok().cloned(),
                channel,
                image_uuid,
                verbose,
                dry_run,
            })
            .await
        }
        Commands::Services { json } => cmd_services(&sapi_url?, json).await,
        Commands::Update => not_yet_implemented("update"),
        Commands::Channel { command } => command.run(),
        Commands::DcMaint { command } => command.run(&sapi_url?).await,
        Commands::Platform { command } => command.run(),
        Commands::PostSetup { command } => {
            command
                .run(commands::PostSetupUrls {
                    sapi_url: sapi_url?,
                    imgapi_url: imgapi_url?,
                    vmapi_url: vmapi_url?,
                    papi_url: papi_url?,
                    napi_url: napi_url?,
                    updates_url: updates_url.clone(),
                    sdc_config,
                })
                .await
        }
        Commands::Experimental { command } => command.run(),
        Commands::Image { command } => command.run(&imgapi_url?, updates_url.as_deref()).await,
        Commands::Dev { command } => command.run(&sapi_url?, &vmapi_url?, &napi_url?).await,
        Commands::Sapi { command } => command.run(&sapi_url?).await,
        Commands::Mahi { command } => command.run(mahi_url, mahi_sitter_url).await,
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

async fn cmd_avail(sapi_url: &str, imgapi_url: &str, json: bool) -> Result<()> {
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;
    let sapi = sapi_client::build_client(sapi_url, false)
        .await
        .context("failed to build SAPI client")?;
    let imgapi = imgapi_client::Client::new_with_client(imgapi_url, http);

    // Get all SAPI services and their current image UUIDs
    let services = sapi
        .list_services()
        .send()
        .await
        .context("failed to list services")?
        .into_inner();

    #[derive(serde::Serialize)]
    struct AvailRow {
        service: String,
        image: String,
        version: String,
    }

    let mut rows: Vec<AvailRow> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    for svc in &services {
        // Get current image_uuid from service params
        let current_image_uuid = svc
            .params
            .as_ref()
            .and_then(|p| p.get("image_uuid"))
            .and_then(|v| v.as_str());

        let current_uuid = match current_image_uuid {
            Some(s) => s.to_string(),
            None => continue,
        };

        // A non-UUID image_uuid in SAPI is a data-corruption signal, not a
        // routine miss — surface it instead of silently dropping the row.
        let parsed_uuid = match sapi_client::Uuid::parse_str(&current_uuid) {
            Ok(u) => u,
            Err(e) => {
                skipped.push((
                    svc.name.clone(),
                    format!("SAPI image_uuid {current_uuid:?} is not a valid UUID: {e}"),
                ));
                continue;
            }
        };

        // Look up current image to get its name. Tolerate 404 (image was
        // deleted from IMGAPI but SAPI still references it — common after
        // pruning) but bail-into-warning on transport / 5xx so the operator
        // can tell "no updates" from "IMGAPI is broken".
        let current_image = match imgapi.get_image().uuid(parsed_uuid).send().await {
            Ok(resp) => resp.into_inner(),
            Err(e) if is_404(&e) => continue,
            Err(e) => {
                skipped.push((svc.name.clone(), format!("IMGAPI get_image failed: {e}")));
                continue;
            }
        };

        // Query IMGAPI for all images with the same name (same rationale
        // as above — list returning empty is not an error, but a transport
        // failure is).
        let candidates = match imgapi.list_images().name(&current_image.name).send().await {
            Ok(resp) => resp.into_inner(),
            Err(e) => {
                skipped.push((svc.name.clone(), format!("IMGAPI list_images failed: {e}")));
                continue;
            }
        };

        // Show images that aren't the currently-installed one
        for img in &candidates {
            if img.uuid.to_string() != current_uuid {
                rows.push(AvailRow {
                    service: svc.name.clone(),
                    image: img.uuid.to_string(),
                    version: format!("{}@{}", img.name, img.version),
                });
            }
        }
    }

    if !skipped.is_empty() {
        eprintln!(
            "warning: {} service(s) skipped due to lookup failures — \
             results below are incomplete:",
            skipped.len()
        );
        for (svc, reason) in &skipped {
            eprintln!("  {svc}: {reason}");
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("Up-to-date.");
    } else {
        println!("{:<24} {:<38} VERSION", "SERVICE", "IMAGE");
        for row in &rows {
            println!("{:<24} {:<38} {}", row.service, row.image, row.version);
        }
    }
    Ok(())
}

async fn cmd_services(sapi_url: &str, json: bool) -> Result<()> {
    let sapi = sapi_client::build_client(sapi_url, false)
        .await
        .context("failed to build SAPI client")?;
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
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;
    let sapi = sapi_client::build_client(sapi_url, false)
        .await
        .context("failed to build SAPI client")?;
    let vmapi = vmapi_client::Client::new_with_client(vmapi_url, http);

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
        // Track VMAPI failures so a wholesale outage doesn't masquerade as
        // a fleet of instances all in unknown state.
        let mut vmapi_errors: Vec<String> = Vec::new();
        for inst in &instances {
            let service_name = svc_name
                .get(&inst.service_uuid)
                .copied()
                .unwrap_or("unknown");

            // Try to get VM details from VMAPI for enrichment. 404 means
            // "SAPI references a VM that no longer exists in VMAPI" (legit
            // stale state); other errors indicate VMAPI itself is broken.
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
                Err(e) if is_404(&e) => ("-".to_string(), "missing".to_string(), "-".to_string()),
                Err(e) => {
                    vmapi_errors.push(format!("{}: {e}", inst.uuid));
                    ("-".to_string(), "?ERR".to_string(), "-".to_string())
                }
            };
            println!(
                "{:<38} {:<20} {:<28} {:<12} {}",
                inst.uuid, service_name, alias, state, image
            );
        }
        if !vmapi_errors.is_empty() {
            eprintln!(
                "warning: VMAPI lookup failed for {} instance(s) — \
                 'STATE' column shows '?ERR' for each:",
                vmapi_errors.len()
            );
            for err in &vmapi_errors {
                eprintln!("  {err}");
            }
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
