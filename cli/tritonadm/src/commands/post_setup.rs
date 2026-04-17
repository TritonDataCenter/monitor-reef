// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use std::io::Write;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::json;

use crate::config::TritonConfig;
use crate::not_yet_implemented;

/// Embedded user-script for zone boot (same as sdcadm's etc/setup/user-script).
const USER_SCRIPT: &str = include_str!("../../etc/setup/user-script");

use crate::DEFAULT_UPDATES_URL;

/// Resolved API URLs and config needed by post-setup commands.
pub struct PostSetupUrls {
    pub sapi_url: String,
    pub imgapi_url: String,
    pub vmapi_url: String,
    pub papi_url: String,
    pub napi_url: String,
    pub updates_url: Option<String>,
    pub sdc_config: Option<TritonConfig>,
}

#[derive(Subcommand)]
pub enum PostSetupCommand {
    /// Create a first CloudAPI instance
    Cloudapi {
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
        /// Dry run (preview without executing)
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Server UUID to place the instance on (default: headnode)
        #[arg(long, short = 's')]
        server: Option<String>,
        /// Image UUID, "latest" (from updates server), or "current" (local only)
        #[arg(long, short = 'i', default_value = "latest")]
        image: String,
        /// Updates server channel (default: from SAPI config or remote default)
        #[arg(long, short = 'C')]
        channel: Option<String>,
    },
    /// Add external NICs to HEAD node SDC services
    CommonExternalNics,
    /// Set up underlay NICs for compute nodes
    UnderlayNics,
    /// Set up HA for binder (ZooKeeper)
    HaBinder,
    /// Set up HA for manatee (PostgreSQL)
    HaManatee,
    /// Initialize fabric networking
    Fabrics,
    /// Make the headnode a provisionable compute node (dev only)
    DevHeadnodeProv,
    /// Load sample data for development (dev only)
    DevSampleData,
    /// Set up Docker service
    Docker,
    /// Set up Container Monitor (CMON) service
    Cmon,
    /// Set up Container Name Service (CNS)
    Cns,
    /// Set up Volumes API (VOLAPI) service
    Volapi,
    /// Set up log archiver service
    Logarchiver,
    /// Set up Key Backup and Management API (KBMAPI)
    Kbmapi,
    /// Set up Prometheus monitoring
    Prometheus,
    /// Create the "grafana" service and a first instance
    Grafana {
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
        /// Dry run (preview without executing)
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Server UUID to place the instance on (default: headnode)
        #[arg(long, short = 's')]
        server: Option<String>,
        /// Image UUID, "latest" (from updates server), or "current" (local only)
        #[arg(long, short = 'i', default_value = "latest")]
        image: String,
        /// Updates server channel (default: from SAPI config or remote default)
        #[arg(long, short = 'C')]
        channel: Option<String>,
    },
    /// Set up firewall logger agent
    FirewallLoggerAgent,
    /// Set up Manta object storage
    Manta,
    /// Set up Portal web UI
    Portal {
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
        /// Dry run (preview without executing)
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Server UUID to place the instance on (default: headnode)
        #[arg(long, short = 's')]
        server: Option<String>,
        /// Image UUID, "latest" (from updates server), or "current" (local only)
        #[arg(long, short = 'i', default_value = "current")]
        image: String,
        /// Updates server channel (default: from SAPI config or remote default)
        #[arg(long, short = 'C')]
        channel: Option<String>,
    },
    /// Create the "triton-api" service and a first instance
    Tritonapi {
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
        /// Dry run (preview without executing)
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Server UUID to place the instance on (default: headnode)
        #[arg(long, short = 's')]
        server: Option<String>,
        /// Image UUID, "latest" (from updates server), or "current" (local only)
        #[arg(long, short = 'i', default_value = "latest")]
        image: String,
        /// Updates server channel (default: from SAPI config or remote default)
        #[arg(long, short = 'C')]
        channel: Option<String>,
    },
}

impl PostSetupCommand {
    pub async fn run(self, urls: PostSetupUrls) -> Result<()> {
        match self {
            Self::Cloudapi {
                yes,
                dry_run,
                server,
                image,
                channel,
            } => {
                cmd_add_service(
                    &CLOUDAPI_CONFIG,
                    &urls,
                    SetupOpts {
                        yes,
                        dry_run,
                        server,
                        image,
                        channel,
                        extra_metadata: None,
                    },
                )
                .await
            }
            Self::CommonExternalNics => cmd_common_external_nics(&urls).await,
            Self::UnderlayNics => not_yet_implemented("post-setup underlay-nics"),
            Self::HaBinder => not_yet_implemented("post-setup ha-binder"),
            Self::HaManatee => not_yet_implemented("post-setup ha-manatee"),
            Self::Fabrics => not_yet_implemented("post-setup fabrics"),
            Self::DevHeadnodeProv => not_yet_implemented("post-setup dev-headnode-prov"),
            Self::DevSampleData => not_yet_implemented("post-setup dev-sample-data"),
            Self::Docker => not_yet_implemented("post-setup docker"),
            Self::Cmon => not_yet_implemented("post-setup cmon"),
            Self::Cns => not_yet_implemented("post-setup cns"),
            Self::Volapi => not_yet_implemented("post-setup volapi"),
            Self::Logarchiver => not_yet_implemented("post-setup logarchiver"),
            Self::Kbmapi => not_yet_implemented("post-setup kbmapi"),
            Self::Prometheus => not_yet_implemented("post-setup prometheus"),
            Self::Grafana {
                yes,
                dry_run,
                server,
                image,
                channel,
            } => {
                cmd_add_service(
                    &GRAFANA_CONFIG,
                    &urls,
                    SetupOpts {
                        yes,
                        dry_run,
                        server,
                        image,
                        channel,
                        extra_metadata: None,
                    },
                )
                .await
            }
            Self::FirewallLoggerAgent => not_yet_implemented("post-setup firewall-logger-agent"),
            Self::Manta => not_yet_implemented("post-setup manta"),
            Self::Portal {
                yes,
                dry_run,
                server,
                image,
                channel,
            } => {
                let extra = build_portal_metadata(&urls).await?;
                cmd_add_service(
                    &PORTAL_CONFIG,
                    &urls,
                    SetupOpts {
                        yes,
                        dry_run,
                        server,
                        image,
                        channel,
                        extra_metadata: Some(extra),
                    },
                )
                .await
            }
            Self::Tritonapi {
                yes,
                dry_run,
                server,
                image,
                channel,
            } => {
                let extra = build_tritonapi_metadata(&urls)?;
                cmd_add_service(
                    &TRITONAPI_CONFIG,
                    &urls,
                    SetupOpts {
                        yes,
                        dry_run,
                        server,
                        image,
                        channel,
                        extra_metadata: Some(extra),
                    },
                )
                .await
            }
        }
    }
}

/// Configuration for a service that can be set up via `post-setup`.
struct ServiceConfig {
    name: &'static str,
    image_name: &'static str,
    package_name: &'static str,
    delegate_dataset: bool,
    firewall_enabled: bool,
    ensure_manta_nic: bool,
}

const CLOUDAPI_CONFIG: ServiceConfig = ServiceConfig {
    name: "cloudapi",
    image_name: "cloudapi",
    package_name: "sdc_1024",
    delegate_dataset: true,
    firewall_enabled: false,
    ensure_manta_nic: false,
};

const GRAFANA_CONFIG: ServiceConfig = ServiceConfig {
    name: "grafana",
    image_name: "grafana",
    package_name: "sdc_1024",
    delegate_dataset: true,
    firewall_enabled: false,
    ensure_manta_nic: true,
};

const PORTAL_CONFIG: ServiceConfig = ServiceConfig {
    name: "portal",
    image_name: "user-portal",
    package_name: "sdc_1024",
    delegate_dataset: false,
    firewall_enabled: true,
    ensure_manta_nic: false,
};

const TRITONAPI_CONFIG: ServiceConfig = ServiceConfig {
    name: "triton-api",
    image_name: "triton-api",
    package_name: "sdc_1024",
    // haproxy needs a persistent /data/tls for the self-signed cert it
    // generates on first boot, so the zone must have a delegated dataset.
    delegate_dataset: true,
    firewall_enabled: true,
    ensure_manta_nic: false,
};

/// Actions determined by the prepare phase.
enum AddServiceAction {
    ImportImage,
    CreateService,
    CreateInstance {
        server_uuid: String,
    },
    ReprovisionInstance {
        inst_uuid: sapi_client::Uuid,
        alias: String,
    },
}

/// Common CLI options for service setup commands.
struct SetupOpts {
    yes: bool,
    dry_run: bool,
    server: Option<String>,
    image: String,
    channel: Option<String>,
    extra_metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Result of finding an image — the manifest and whether it needs downloading.
struct ImageSelection {
    image: imgapi_client::types::Image,
    needs_download: bool,
}

/// Build portal-specific SAPI metadata.
///
/// Generates a JWT secret, reads the admin SSH key and fingerprint from
/// the headnode, and builds a single-datacenter entry pointing at the
/// local CloudAPI.
async fn build_portal_metadata(
    urls: &PostSetupUrls,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut meta = serde_json::Map::new();

    // Generate a random JWT secret (64 hex chars)
    let jwt_secret = generate_hex_secret(32).await?;
    meta.insert("USER_PORTAL_JWT_SECRET".into(), json!(jwt_secret));

    // Get admin SSH key fingerprint from headnode
    let key_id = get_ssh_key_fingerprint().context(
        "failed to get admin SSH key fingerprint; \
         ensure /root/.ssh/sdc.id_rsa.pub exists on the headnode",
    )?;
    meta.insert("USER_PORTAL_KEY_ID".into(), json!(key_id));

    // Read the private key so config-agent can render it into the zone
    // (sapi_manifests/sdc-key/template → /opt/smartdc/portal/etc/sdc_key)
    let sdc_key = tokio::fs::read_to_string("/root/.ssh/sdc.id_rsa")
        .await
        .context(
            "failed to read /root/.ssh/sdc.id_rsa; \
             ensure the admin SSH private key exists on the headnode",
        )?;
    meta.insert("USER_PORTAL_SDC_KEY".into(), json!(sdc_key));

    // Build datacenters array from SDC config
    if let Some(cfg) = &urls.sdc_config {
        let cloudapi_url = cfg.service_url("cloudapi");
        meta.insert(
            "USER_PORTAL_DATACENTERS".into(),
            json!([{
                "name": cfg.datacenter_name,
                "url": cloudapi_url,
                "last": true,
            }]),
        );
    }

    Ok(meta)
}

/// Build triton-api-specific SAPI metadata.
///
/// The triton-gateway SAPI template references `{{{CLOUDAPI_SERVICE}}}` to
/// construct the CloudAPI URL for request proxying.
fn build_tritonapi_metadata(
    urls: &PostSetupUrls,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut meta = serde_json::Map::new();

    let sdc_config = urls
        .sdc_config
        .as_ref()
        .context("SDC config required to determine CloudAPI service domain")?;
    let cloudapi_domain = format!(
        "cloudapi.{}.{}",
        sdc_config.datacenter_name, sdc_config.dns_domain
    );

    meta.insert(
        "CLOUDAPI_SERVICE".into(),
        serde_json::Value::String(cloudapi_domain),
    );

    Ok(meta)
}

/// Generate a hex-encoded random secret of the given byte length.
async fn generate_hex_secret(bytes: usize) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; bytes];
    let mut f = tokio::fs::File::open("/dev/urandom")
        .await
        .context("failed to open /dev/urandom")?;
    f.read_exact(&mut buf)
        .await
        .context("failed to read from /dev/urandom")?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Read the admin SSH key fingerprint from the headnode.
fn get_ssh_key_fingerprint() -> Result<String> {
    let output = std::process::Command::new("ssh-keygen")
        .args(["-l", "-f", "/root/.ssh/sdc.id_rsa.pub", "-E", "md5"])
        .output()
        .context("failed to run ssh-keygen")?;
    if !output.status.success() {
        anyhow::bail!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Output format: "2048 MD5:xx:xx:xx:... comment (RSA)"
    // We want the "MD5:xx:xx:xx:..." part, but CloudAPI expects just the
    // fingerprint without the "MD5:" prefix for MD5 format, or the full
    // SHA256 fingerprint. Let's use the full field as-is.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let fingerprint = stdout
        .split_whitespace()
        .nth(1)
        .context("unexpected ssh-keygen output format")?
        .to_string();
    Ok(fingerprint)
}

async fn cmd_add_service(
    config: &ServiceConfig,
    urls: &PostSetupUrls,
    opts: SetupOpts,
) -> Result<()> {
    let SetupOpts {
        yes,
        dry_run,
        server: server_opt,
        image: image_arg,
        channel: channel_opt,
        extra_metadata,
    } = opts;
    // Build shared HTTP client
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;

    let sapi = sapi_client::Client::new_with_client(&urls.sapi_url, http.clone());
    let local_imgapi = imgapi_client::Client::new_with_client(&urls.imgapi_url, http.clone());
    let local_imgapi_typed =
        imgapi_client::TypedClient::new_with_client(&urls.imgapi_url, http.clone());
    let vmapi = vmapi_client::TypedClient::new_with_client(&urls.vmapi_url, http.clone());
    let papi = papi_client::Client::new_with_client(&urls.papi_url, http.clone());
    let napi = napi_client::Client::new_with_client(&urls.napi_url, http.clone());

    // Updates server client (another IMGAPI instance)
    let updates_url = urls.updates_url.as_deref().unwrap_or(DEFAULT_UPDATES_URL);
    let updates_imgapi = imgapi_client::Client::new_with_client(updates_url, http);

    // ── Phase 1: Gather information ──

    // Get the "sdc" application for datacenter metadata
    let apps = sapi
        .list_applications()
        .name("sdc")
        .send()
        .await
        .context("failed to list applications")?
        .into_inner();
    let sdc_app = apps.first().context("no 'sdc' application found in SAPI")?;

    let sdc_metadata = sdc_app
        .metadata
        .as_ref()
        .context("sdc application has no metadata")?;
    let datacenter_name = sdc_metadata
        .get("datacenter_name")
        .and_then(|v| v.as_str())
        .context("sdc metadata missing datacenter_name")?;
    let dns_domain = sdc_metadata
        .get("dns_domain")
        .and_then(|v| v.as_str())
        .context("sdc metadata missing dns_domain")?;
    let service_domain = format!("{}.{datacenter_name}.{dns_domain}", config.name);

    // Resolve channel for updates server queries
    let channel = resolve_channel(channel_opt, sdc_metadata, &updates_imgapi).await?;

    // Look up the package
    let packages = papi
        .list_packages()
        .name(config.package_name)
        .active(true)
        .send()
        .await
        .with_context(|| format!("failed to list '{}' packages", config.package_name))?
        .into_inner();
    let pkg = match packages.len() {
        1 => &packages[0],
        0 => anyhow::bail!("no active '{}' package found in PAPI", config.package_name),
        n => anyhow::bail!(
            "{n} '{}' packages found in PAPI, expected exactly 1",
            config.package_name
        ),
    };
    let billing_id = pkg.uuid.to_string();

    // Check if the service already exists
    let services = sapi
        .list_services()
        .name(config.name)
        .application_uuid(sdc_app.uuid)
        .send()
        .await
        .with_context(|| format!("failed to list '{}' services", config.name))?
        .into_inner();
    let existing_svc = services.first();

    // Check existing instances (if service exists)
    let existing_instances = if let Some(svc) = existing_svc {
        sapi.list_instances()
            .service_uuid(svc.uuid)
            .send()
            .await
            .context("failed to list instances")?
            .into_inner()
    } else {
        Vec::new()
    };
    let existing_inst = existing_instances.first();

    // Get VM state if instance exists (to check current image)
    let existing_vm = if let Some(inst) = existing_inst {
        match vmapi.inner().get_vm().uuid(inst.uuid).send().await {
            Ok(resp) => Some(resp.into_inner()),
            Err(_) => None,
        }
    } else {
        None
    };

    // Find the target image (local or remote)
    let selection = find_image(
        &local_imgapi,
        &updates_imgapi,
        config.image_name,
        &image_arg,
        &channel,
    )
    .await?;
    let target_image = &selection.image;

    // Resolve server UUID
    let server_uuid = match server_opt {
        Some(s) => {
            uuid::Uuid::parse_str(&s).context("--server must be a valid UUID")?;
            s
        }
        None => urls
            .sdc_config
            .as_ref()
            .and_then(|c| c.server_uuid.clone())
            .context("cannot determine headnode UUID: set --server or run on a Triton headnode")?,
    };

    // ── Phase 2: Determine actions ──

    let mut actions: Vec<AddServiceAction> = Vec::new();

    if selection.needs_download {
        actions.push(AddServiceAction::ImportImage);
    }

    if existing_svc.is_none() {
        actions.push(AddServiceAction::CreateService);
    }

    match existing_inst {
        None => {
            actions.push(AddServiceAction::CreateInstance {
                server_uuid: server_uuid.clone(),
            });
        }
        Some(inst) if existing_instances.len() == 1 => {
            if let Some(vm) = &existing_vm
                && vm.image_uuid.map(|u| u.to_string()) != Some(target_image.uuid.to_string())
            {
                let alias = vm.alias.clone().unwrap_or_default();
                actions.push(AddServiceAction::ReprovisionInstance {
                    inst_uuid: inst.uuid,
                    alias,
                });
            }
        }
        _ => {}
    }

    if actions.is_empty() {
        eprintln!(
            "Nothing to do — {} service and instance are up to date.",
            config.name
        );
        return Ok(());
    }

    // ── Phase 3: Summarize and confirm ──

    eprintln!("The following changes will be made:");
    for action in &actions {
        match action {
            AddServiceAction::ImportImage => {
                eprintln!(
                    "  - Import image {} ({}@{})\n    \
                     from updates server using channel \"{channel}\"",
                    target_image.uuid, target_image.name, target_image.version
                );
            }
            AddServiceAction::CreateService => {
                eprintln!("  - Create \"{}\" service in SAPI", config.name);
            }
            AddServiceAction::CreateInstance { server_uuid } => {
                eprintln!(
                    "  - Create \"{}\" instance on server {server_uuid}\n    \
                     with image {} ({}@{})",
                    config.name, target_image.uuid, target_image.name, target_image.version
                );
            }
            AddServiceAction::ReprovisionInstance { inst_uuid, alias } => {
                eprintln!(
                    "  - Reprovision instance {inst_uuid} ({alias})\n    \
                     with image {} ({}@{})",
                    target_image.uuid, target_image.name, target_image.version
                );
            }
        }
    }

    if dry_run {
        eprintln!("Dry run — no changes made.");
        return Ok(());
    }

    if !yes {
        eprint!("Would you like to continue? [y/N] ");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            eprintln!("Aborted.");
            return Ok(());
        }
    }

    // ── Phase 4: Execute ──

    let mut svc_uuid = existing_svc.map(|s| s.uuid);

    for action in &actions {
        match action {
            AddServiceAction::ImportImage => {
                eprintln!(
                    "Importing image {} ({}@{})...",
                    target_image.uuid, target_image.name, target_image.version
                );
                let source_url = format!("{updates_url}?channel={channel}");
                local_imgapi_typed
                    .import_remote_image(&target_image.uuid, &source_url, true)
                    .await
                    .context("failed to import image from updates server")?;

                // Poll until the image is active
                wait_for_image_active(&local_imgapi, target_image.uuid).await?;
                eprintln!("Image imported.");
            }
            AddServiceAction::CreateService => {
                eprintln!("Creating \"{}\" service...", config.name);
                let mut params = serde_json::Map::new();
                params.insert("billing_id".into(), json!(billing_id));
                params.insert("image_uuid".into(), json!(target_image.uuid.to_string()));
                params.insert("archive_on_delete".into(), json!(true));
                params.insert("delegate_dataset".into(), json!(config.delegate_dataset));
                params.insert("maintain_resolvers".into(), json!(true));
                params.insert(
                    "networks".into(),
                    json!([
                        {"name": "admin"},
                        {"name": "external", "primary": true}
                    ]),
                );
                params.insert("firewall_enabled".into(), json!(config.firewall_enabled));
                params.insert(
                    "tags".into(),
                    json!({"smartdc_role": config.name, "smartdc_type": "core"}),
                );

                let mut metadata = serde_json::Map::new();
                metadata.insert("SERVICE_NAME".into(), json!(config.name));
                metadata.insert("SERVICE_DOMAIN".into(), json!(service_domain));
                metadata.insert("user-script".into(), json!(USER_SCRIPT));
                if let Some(ref extra) = extra_metadata {
                    for (k, v) in extra {
                        metadata.insert(k.clone(), v.clone());
                    }
                }

                let svc = sapi
                    .create_service()
                    .body(sapi_client::types::CreateServiceBody {
                        name: config.name.into(),
                        application_uuid: sdc_app.uuid,
                        params: Some(params),
                        metadata: Some(metadata),
                        type_: Some(sapi_client::types::ServiceType::Vm),
                        uuid: None,
                        manifests: None,
                        master: None,
                    })
                    .send()
                    .await
                    .with_context(|| format!("failed to create {} service", config.name))?
                    .into_inner();
                eprintln!("Created service {} ({})", svc.uuid, svc.name);
                svc_uuid = Some(svc.uuid);
            }
            AddServiceAction::CreateInstance { server_uuid } => {
                let svc_id =
                    svc_uuid.context("service UUID not available for instance creation")?;
                eprintln!(
                    "Creating \"{}\" instance on server {server_uuid}...",
                    config.name
                );

                let mut inst_params = serde_json::Map::new();
                inst_params.insert("alias".into(), json!(format!("{}0", config.name)));
                inst_params.insert("server_uuid".into(), json!(server_uuid));

                let inst = sapi
                    .create_instance()
                    .body(sapi_client::types::CreateInstanceBody {
                        service_uuid: svc_id,
                        params: Some(inst_params),
                        metadata: None,
                        manifests: None,
                        uuid: None,
                        master: None,
                    })
                    .send()
                    .await
                    .with_context(|| format!("failed to create {} instance", config.name))?
                    .into_inner();
                eprintln!("Created instance {}", inst.uuid);

                // Ensure manta NIC on the newly created instance (if configured).
                // Non-fatal: warn and continue if the NIC can't be added.
                if config.ensure_manta_nic {
                    ensure_manta_nic(&napi, &vmapi, inst.uuid, config.name)
                        .await
                        .unwrap_or_else(|e| eprintln!("Warning: manta NIC setup: {e}"));
                }
            }
            AddServiceAction::ReprovisionInstance { inst_uuid, alias } => {
                eprintln!("Reprovisioning instance {inst_uuid} ({alias})...");
                sapi.upgrade_instance()
                    .uuid(*inst_uuid)
                    .body(sapi_client::types::UpgradeInstanceBody {
                        image_uuid: target_image.uuid,
                    })
                    .send()
                    .await
                    .with_context(|| format!("failed to reprovision {} instance", config.name))?;
                eprintln!("Reprovisioned instance {inst_uuid}");
            }
        }
    }

    // If we didn't just create the instance (which already handles manta NIC),
    // check manta NIC on existing instances
    if config.ensure_manta_nic
        && let Some(inst) = existing_inst
        && !actions
            .iter()
            .any(|a| matches!(a, AddServiceAction::CreateInstance { .. }))
    {
        ensure_manta_nic(&napi, &vmapi, inst.uuid, config.name)
            .await
            .unwrap_or_else(|e| eprintln!("Warning: manta NIC setup: {e}"));
    }

    eprintln!("Done.");
    Ok(())
}

/// Resolve the updates channel.
///
/// Priority: --channel flag > SAPI sdc metadata `update_channel` > remote default.
async fn resolve_channel(
    channel_opt: Option<String>,
    sdc_metadata: &serde_json::Map<String, serde_json::Value>,
    updates_imgapi: &imgapi_client::Client,
) -> Result<String> {
    // 1. Explicit --channel flag
    if let Some(ch) = channel_opt {
        return Ok(ch);
    }

    // 2. SAPI sdc application metadata
    if let Some(ch) = sdc_metadata.get("update_channel").and_then(|v| v.as_str()) {
        return Ok(ch.to_string());
    }

    // 3. Query updates server for default channel
    match updates_imgapi.list_channels().send().await {
        Ok(resp) => {
            let channels = resp.into_inner();
            for ch in &channels {
                if ch.default == Some(true) {
                    return Ok(ch.name.clone());
                }
            }
            if let Some(first) = channels.first() {
                return Ok(first.name.clone());
            }
            anyhow::bail!(
                "updates server has no channels configured; \
                 use --channel to specify one"
            )
        }
        Err(e) => {
            anyhow::bail!(
                "failed to query updates server for default channel: {e}\n\
                 Use --channel to specify one, or --image current to skip the updates server"
            )
        }
    }
}

/// Find the target image, checking local IMGAPI and/or the updates server.
async fn find_image(
    local_imgapi: &imgapi_client::Client,
    updates_imgapi: &imgapi_client::Client,
    image_name: &str,
    image_arg: &str,
    channel: &str,
) -> Result<ImageSelection> {
    match image_arg {
        "current" => {
            // Local IMGAPI only
            let images = local_imgapi
                .list_images()
                .name(image_name)
                .send()
                .await
                .with_context(|| format!("failed to list '{image_name}' images in local IMGAPI"))?
                .into_inner();

            if images.is_empty() {
                anyhow::bail!(
                    "no '{image_name}' images found in local IMGAPI; \
                     use --image latest to fetch from the updates server"
                );
            }

            Ok(ImageSelection {
                image: pick_latest(images)?,
                needs_download: false,
            })
        }
        "latest" => {
            // Query updates server for the latest image
            let remote_images = updates_imgapi
                .list_images()
                .name(image_name)
                .channel(channel)
                .send()
                .await
                .with_context(|| format!("failed to list '{image_name}' images on updates server"))?
                .into_inner();

            if remote_images.is_empty() {
                anyhow::bail!(
                    "no '{image_name}' images found on updates server (channel: {channel}); \
                     try --image current to use a locally-available image"
                );
            }

            let best = pick_latest(remote_images)?;

            // Check if it already exists locally
            let needs_download = local_imgapi
                .get_image()
                .uuid(best.uuid)
                .send()
                .await
                .is_err();

            Ok(ImageSelection {
                image: best,
                needs_download,
            })
        }
        uuid_str => {
            // Treat as UUID — try local first, fall back to remote
            let uuid = uuid::Uuid::parse_str(uuid_str)
                .context("--image must be 'latest', 'current', or a valid UUID")?;

            match local_imgapi.get_image().uuid(uuid).send().await {
                Ok(resp) => Ok(ImageSelection {
                    image: resp.into_inner(),
                    needs_download: false,
                }),
                Err(_) => {
                    // Try updates server
                    let image = updates_imgapi
                        .get_image()
                        .uuid(uuid)
                        .channel(channel)
                        .send()
                        .await
                        .context("image not found in local IMGAPI or updates server")?
                        .into_inner();
                    Ok(ImageSelection {
                        image,
                        needs_download: true,
                    })
                }
            }
        }
    }
}

/// Pick the image with the latest published_at from a list.
fn pick_latest(images: Vec<imgapi_client::types::Image>) -> Result<imgapi_client::types::Image> {
    images
        .into_iter()
        .max_by(|a, b| a.published_at.cmp(&b.published_at))
        .context("no images to choose from")
}

/// Poll local IMGAPI until the image reaches "active" state.
///
/// The import-remote action is async — the image may not exist yet when
/// we start polling (404), then appear as "unactivated", then become "active".
async fn wait_for_image_active(imgapi: &imgapi_client::Client, uuid: uuid::Uuid) -> Result<()> {
    for _ in 0..120 {
        match imgapi.get_image().uuid(uuid).send().await {
            Ok(resp) => {
                if resp.into_inner().state == imgapi_client::types::ImageState::Active {
                    return Ok(());
                }
            }
            Err(_) => {
                // Image may not exist yet (404) — keep polling
            }
        }
        eprint!(".");
        std::io::stderr().flush().ok();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    anyhow::bail!("timed out waiting for image {uuid} to become active (4 minutes)")
}

/// Add external NICs to adminui and imgapi zones.
///
/// Matches sdcadm's `post-setup common-external-nics`. Required before
/// IMGAPI can reach the updates server to import images.
async fn cmd_common_external_nics(urls: &PostSetupUrls) -> Result<()> {
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;

    let sapi = sapi_client::Client::new_with_client(&urls.sapi_url, http.clone());
    let vmapi = vmapi_client::TypedClient::new_with_client(&urls.vmapi_url, http.clone());
    let napi = napi_client::Client::new_with_client(&urls.napi_url, http);

    // Find the external network
    let networks = napi
        .list_networks()
        .name("external")
        .send()
        .await
        .context("failed to list networks")?
        .into_inner();
    let external_net = networks
        .first()
        .context("no 'external' network found in NAPI")?;
    let net_uuid = uuid::Uuid::parse_str(&external_net.uuid.to_string())
        .context("failed to parse external network UUID")?;

    let svc_names = ["imgapi", "adminui"];
    let mut changed = false;

    for svc_name in &svc_names {
        let instances = get_service_instances(&sapi, svc_name).await?;
        for inst in &instances {
            if add_nic_if_missing(
                &napi, &vmapi, inst.uuid, "external", net_uuid, true, svc_name,
            )
            .await?
            {
                changed = true;
            }
        }
    }

    if !changed {
        eprintln!("All imgapi and adminui instances already have external NICs.");
    }
    Ok(())
}

/// Get instances of a named service from SAPI.
pub async fn get_service_instances(
    sapi: &sapi_client::Client,
    svc_name: &str,
) -> Result<Vec<sapi_client::types::Instance>> {
    let services = sapi
        .list_services()
        .name(svc_name)
        .send()
        .await
        .with_context(|| format!("failed to list services for {svc_name}"))?
        .into_inner();
    let svc = match services.first() {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    let instances = sapi
        .list_instances()
        .service_uuid(svc.uuid)
        .send()
        .await
        .with_context(|| format!("failed to list instances for {svc_name}"))?
        .into_inner();
    Ok(instances)
}

/// Add a NIC to an instance if it doesn't already have one with the given nic_tag.
/// Returns true if a NIC was added.
async fn add_nic_if_missing(
    napi: &napi_client::Client,
    vmapi: &vmapi_client::TypedClient,
    inst_uuid: sapi_client::Uuid,
    nic_tag: &str,
    net_uuid: uuid::Uuid,
    primary: bool,
    svc_name: &str,
) -> Result<bool> {
    let nics = napi
        .list_nics()
        .belongs_to_uuid(inst_uuid.to_string())
        .send()
        .await
        .with_context(|| format!("failed to list NICs for {svc_name} instance {inst_uuid}"))?
        .into_inner();

    if nics
        .iter()
        .any(|nic| nic.nic_tag.as_deref() == Some(nic_tag))
    {
        return Ok(false);
    }

    eprintln!("Adding {nic_tag} NIC to {svc_name} instance {inst_uuid}...");
    let net_entry = if primary {
        json!({"uuid": net_uuid.to_string(), "primary": true})
    } else {
        json!(net_uuid.to_string())
    };
    vmapi
        .add_nics(
            &inst_uuid,
            &vmapi_client::AddNicsRequest {
                networks: Some(vec![net_entry]),
                macs: None,
            },
        )
        .await
        .with_context(|| {
            format!("failed to add {nic_tag} NIC to {svc_name} instance {inst_uuid}")
        })?;
    eprintln!("Added {nic_tag} NIC to {svc_name} instance {inst_uuid}.");
    Ok(true)
}

/// Ensure an instance has a NIC on the manta network.
///
/// Returns Err on genuine failures (NAPI unreachable, malformed network
/// UUID, `add_nics` failure). Callers treat failures as non-fatal by
/// logging and continuing.
async fn ensure_manta_nic(
    napi: &napi_client::Client,
    vmapi: &vmapi_client::TypedClient,
    inst_uuid: sapi_client::Uuid,
    svc_name: &str,
) -> Result<()> {
    let manta_networks = napi
        .list_networks()
        .name("manta")
        .send()
        .await
        .context("failed to list manta networks")?
        .into_inner();
    let Some(manta_net) = manta_networks.first() else {
        eprintln!("No manta network found — skipping manta NIC.");
        return Ok(());
    };

    let manta_uuid = uuid::Uuid::parse_str(&manta_net.uuid.to_string())
        .context("failed to parse manta network UUID")?;

    add_nic_if_missing(napi, vmapi, inst_uuid, "manta", manta_uuid, false, svc_name).await?;
    Ok(())
}
