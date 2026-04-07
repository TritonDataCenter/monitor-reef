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

/// Resolved API URLs and config needed by post-setup commands.
pub struct PostSetupUrls {
    pub sapi_url: String,
    pub imgapi_url: String,
    pub vmapi_url: String,
    pub papi_url: String,
    pub napi_url: String,
    pub sdc_config: Option<TritonConfig>,
}

#[derive(Subcommand)]
pub enum PostSetupCommand {
    /// Set up CloudAPI
    Cloudapi,
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
        /// Image UUID or "latest" (default: latest in local IMGAPI)
        #[arg(long, short = 'i', default_value = "latest")]
        image: String,
    },
    /// Set up firewall logger agent
    FirewallLoggerAgent,
    /// Set up Manta object storage
    Manta,
    /// Set up Portal web UI
    Portal,
}

impl PostSetupCommand {
    pub async fn run(self, urls: PostSetupUrls) -> Result<()> {
        match self {
            Self::Cloudapi => not_yet_implemented("post-setup cloudapi"),
            Self::CommonExternalNics => not_yet_implemented("post-setup common-external-nics"),
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
            } => cmd_post_setup_grafana(urls, yes, dry_run, server, image).await,
            Self::FirewallLoggerAgent => not_yet_implemented("post-setup firewall-logger-agent"),
            Self::Manta => not_yet_implemented("post-setup manta"),
            Self::Portal => not_yet_implemented("post-setup portal"),
        }
    }
}

/// Actions determined by the prepare phase.
enum GrafanaAction {
    CreateService,
    CreateInstance {
        server_uuid: String,
    },
    ReprovisionInstance {
        inst_uuid: sapi_client::Uuid,
        alias: String,
    },
}

async fn cmd_post_setup_grafana(
    urls: PostSetupUrls,
    yes: bool,
    dry_run: bool,
    server_opt: Option<String>,
    image_arg: String,
) -> Result<()> {
    // Build shared HTTP client
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;

    let sapi = sapi_client::Client::new_with_client(&urls.sapi_url, http.clone());
    let imgapi = imgapi_client::Client::new_with_client(&urls.imgapi_url, http.clone());
    let vmapi = vmapi_client::TypedClient::new_with_client(&urls.vmapi_url, http.clone());
    let papi = papi_client::Client::new_with_client(&urls.papi_url, http.clone());
    let napi = napi_client::Client::new_with_client(&urls.napi_url, http);

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
    let service_domain = format!("grafana.{datacenter_name}.{dns_domain}");

    // Look up sdc_1024 package
    let packages = papi
        .list_packages()
        .name("sdc_1024")
        .active(true)
        .send()
        .await
        .context("failed to list packages")?
        .into_inner();
    let pkg = match packages.len() {
        1 => &packages[0],
        0 => anyhow::bail!("no active 'sdc_1024' package found in PAPI"),
        n => anyhow::bail!("{n} 'sdc_1024' packages found in PAPI, expected exactly 1"),
    };
    let billing_id = pkg.uuid.to_string();

    // Check if grafana service already exists
    let services = sapi
        .list_services()
        .name("grafana")
        .application_uuid(sdc_app.uuid)
        .send()
        .await
        .context("failed to list services")?
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

    // Find grafana image
    let target_image = find_image(&imgapi, &image_arg).await?;

    // Resolve server UUID
    let server_uuid = match server_opt {
        Some(s) => {
            // Validate it looks like a UUID
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

    let mut actions: Vec<GrafanaAction> = Vec::new();

    if existing_svc.is_none() {
        actions.push(GrafanaAction::CreateService);
    }

    match existing_inst {
        None => {
            actions.push(GrafanaAction::CreateInstance {
                server_uuid: server_uuid.clone(),
            });
        }
        Some(inst) if existing_instances.len() == 1 => {
            // Check if the single instance needs reprovisioning
            if let Some(vm) = &existing_vm
                && vm.image_uuid.map(|u| u.to_string()) != Some(target_image.uuid.to_string())
            {
                let alias = vm.alias.clone().unwrap_or_default();
                actions.push(GrafanaAction::ReprovisionInstance {
                    inst_uuid: inst.uuid,
                    alias,
                });
            }
        }
        _ => {}
    }

    if actions.is_empty() {
        eprintln!("Nothing to do — grafana service and instance are up to date.");
        return Ok(());
    }

    // ── Phase 3: Summarize and confirm ──

    eprintln!("The following changes will be made:");
    for action in &actions {
        match action {
            GrafanaAction::CreateService => {
                eprintln!("  - Create \"grafana\" service in SAPI");
            }
            GrafanaAction::CreateInstance { server_uuid } => {
                eprintln!(
                    "  - Create \"grafana\" instance on server {server_uuid}\n    \
                     with image {} ({}@{})",
                    target_image.uuid, target_image.name, target_image.version
                );
            }
            GrafanaAction::ReprovisionInstance { inst_uuid, alias } => {
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
            GrafanaAction::CreateService => {
                eprintln!("Creating \"grafana\" service...");
                let mut params = serde_json::Map::new();
                params.insert("billing_id".into(), json!(billing_id));
                params.insert("image_uuid".into(), json!(target_image.uuid.to_string()));
                params.insert("archive_on_delete".into(), json!(true));
                params.insert("delegate_dataset".into(), json!(true));
                params.insert("maintain_resolvers".into(), json!(true));
                params.insert(
                    "networks".into(),
                    json!([
                        {"name": "admin"},
                        {"name": "external", "primary": true}
                    ]),
                );
                params.insert("firewall_enabled".into(), json!(false));
                params.insert(
                    "tags".into(),
                    json!({"smartdc_role": "grafana", "smartdc_type": "core"}),
                );

                let mut metadata = serde_json::Map::new();
                metadata.insert("SERVICE_NAME".into(), json!("grafana"));
                metadata.insert("SERVICE_DOMAIN".into(), json!(service_domain));
                metadata.insert("user-script".into(), json!(USER_SCRIPT));

                let svc = sapi
                    .create_service()
                    .body(sapi_client::types::CreateServiceBody {
                        name: "grafana".into(),
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
                    .context("failed to create grafana service")?
                    .into_inner();
                eprintln!("Created service {} ({})", svc.uuid, svc.name);
                svc_uuid = Some(svc.uuid);
            }
            GrafanaAction::CreateInstance { server_uuid } => {
                let svc_id =
                    svc_uuid.context("service UUID not available for instance creation")?;
                eprintln!("Creating \"grafana\" instance on server {server_uuid}...");

                let mut inst_params = serde_json::Map::new();
                inst_params.insert("alias".into(), json!("grafana0"));
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
                    .context("failed to create grafana instance")?
                    .into_inner();
                eprintln!("Created instance {}", inst.uuid);

                // Ensure manta NIC on the newly created instance
                ensure_manta_nic(&napi, &vmapi, inst.uuid).await;
            }
            GrafanaAction::ReprovisionInstance { inst_uuid, alias } => {
                eprintln!("Reprovisioning instance {inst_uuid} ({alias})...");
                sapi.upgrade_instance()
                    .uuid(*inst_uuid)
                    .body(sapi_client::types::UpgradeInstanceBody {
                        image_uuid: target_image.uuid,
                    })
                    .send()
                    .await
                    .context("failed to reprovision grafana instance")?;
                eprintln!("Reprovisioned instance {inst_uuid}");
            }
        }
    }

    // If we didn't just create the instance (which already handles manta NIC),
    // check manta NIC on existing instances
    if let Some(inst) = existing_inst
        && !actions
            .iter()
            .any(|a| matches!(a, GrafanaAction::CreateInstance { .. }))
    {
        ensure_manta_nic(&napi, &vmapi, inst.uuid).await;
    }

    eprintln!("Done.");
    Ok(())
}

/// Find the target grafana image in local IMGAPI.
async fn find_image(
    imgapi: &imgapi_client::Client,
    image_arg: &str,
) -> Result<imgapi_client::types::Image> {
    if image_arg == "latest" || image_arg == "current" {
        let images = imgapi
            .list_images()
            .name("grafana")
            .send()
            .await
            .context("failed to list grafana images in IMGAPI")?
            .into_inner();

        if images.is_empty() {
            anyhow::bail!(
                "no 'grafana' images found in IMGAPI — import one first \
                 (e.g., via imgapi-cli or sdc-imgadm)"
            );
        }

        // Pick the latest by published_at
        let mut best = &images[0];
        for img in &images[1..] {
            if img.published_at > best.published_at {
                best = img;
            }
        }
        Ok(best.clone())
    } else {
        // Treat as UUID
        let uuid = uuid::Uuid::parse_str(image_arg)
            .context("--image must be 'latest', 'current', or a valid UUID")?;
        let image = imgapi
            .get_image()
            .uuid(uuid)
            .send()
            .await
            .context("failed to get image from IMGAPI")?
            .into_inner();
        Ok(image)
    }
}

/// Ensure the grafana instance has a NIC on the manta network (non-fatal).
async fn ensure_manta_nic(
    napi: &napi_client::Client,
    vmapi: &vmapi_client::TypedClient,
    inst_uuid: sapi_client::Uuid,
) {
    // Check if manta network exists
    let manta_networks = match napi.list_networks().name("manta").send().await {
        Ok(resp) => resp.into_inner(),
        Err(_) => return, // Can't reach NAPI — skip silently
    };
    let manta_net = match manta_networks.first() {
        Some(net) => net,
        None => {
            eprintln!("No manta network found — skipping manta NIC.");
            return;
        }
    };

    // Check if instance already has a manta NIC
    let nics = match napi
        .list_nics()
        .belongs_to_uuid(inst_uuid.to_string())
        .send()
        .await
    {
        Ok(resp) => resp.into_inner(),
        Err(_) => return,
    };

    let has_manta_nic = nics
        .iter()
        .any(|nic| nic.nic_tag.as_deref() == Some("manta"));
    if has_manta_nic {
        return;
    }

    // Add manta NIC
    eprintln!("Adding manta NIC to instance {inst_uuid}...");
    let manta_uuid = uuid::Uuid::parse_str(&manta_net.uuid.to_string());
    let manta_uuid = match manta_uuid {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Warning: failed to parse manta network UUID: {e}");
            return;
        }
    };
    match vmapi
        .add_nics(
            &inst_uuid,
            &vmapi_client::AddNicsRequest {
                networks: Some(vec![manta_uuid]),
                macs: None,
            },
        )
        .await
    {
        Ok(_) => eprintln!("Added manta NIC to instance {inst_uuid}"),
        Err(e) => eprintln!("Warning: failed to add manta NIC: {e}"),
    }
}
