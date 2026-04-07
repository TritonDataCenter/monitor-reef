// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Development helper commands for iterating on tritonadm.
//!
//! These are not part of sdcadm and exist purely for development convenience.

use anyhow::{Context, Result};
use clap::Subcommand;

use super::post_setup::get_service_instances;

#[derive(Subcommand)]
pub enum DevCommand {
    /// Remove external NICs from imgapi and adminui (undo common-external-nics)
    RemoveExternalNics,
    /// Remove grafana service and instance (undo post-setup grafana)
    RemoveGrafana,
}

impl DevCommand {
    pub async fn run(self, sapi_url: &str, vmapi_url: &str, napi_url: &str) -> Result<()> {
        match self {
            Self::RemoveExternalNics => {
                cmd_remove_external_nics(sapi_url, vmapi_url, napi_url).await
            }
            Self::RemoveGrafana => cmd_remove_grafana(sapi_url, vmapi_url).await,
        }
    }
}

async fn cmd_remove_external_nics(sapi_url: &str, vmapi_url: &str, napi_url: &str) -> Result<()> {
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;

    let sapi = sapi_client::Client::new_with_client(sapi_url, http.clone());
    let vmapi = vmapi_client::TypedClient::new_with_client(vmapi_url, http.clone());
    let napi = napi_client::Client::new_with_client(napi_url, http);

    let svc_names = ["imgapi", "adminui"];
    let mut removed = false;

    for svc_name in &svc_names {
        let instances = get_service_instances(&sapi, svc_name).await?;
        for inst in &instances {
            let nics = napi
                .list_nics()
                .belongs_to_uuid(inst.uuid.to_string())
                .send()
                .await
                .with_context(|| format!("failed to list NICs for {svc_name} {}", inst.uuid))?
                .into_inner();

            let external_macs: Vec<String> = nics
                .iter()
                .filter(|nic| nic.nic_tag.as_deref() == Some("external"))
                .map(|nic| nic.mac.clone())
                .collect();

            if external_macs.is_empty() {
                eprintln!("{svc_name} instance {} has no external NIC.", inst.uuid);
                continue;
            }

            eprintln!(
                "Removing external NIC(s) from {svc_name} instance {}...",
                inst.uuid
            );
            vmapi
                .remove_nics(&inst.uuid, external_macs)
                .await
                .with_context(|| {
                    format!(
                        "failed to remove external NIC from {svc_name} instance {}",
                        inst.uuid
                    )
                })?;
            eprintln!(
                "Removed external NIC from {svc_name} instance {}.",
                inst.uuid
            );
            removed = true;
        }
    }

    if !removed {
        eprintln!("No external NICs found to remove.");
    }
    Ok(())
}

async fn cmd_remove_grafana(sapi_url: &str, vmapi_url: &str) -> Result<()> {
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;

    let sapi = sapi_client::Client::new_with_client(sapi_url, http.clone());
    let vmapi = vmapi_client::TypedClient::new_with_client(vmapi_url, http);

    // Find grafana service
    let services = sapi
        .list_services()
        .name("grafana")
        .send()
        .await
        .context("failed to list services")?
        .into_inner();

    let svc = match services.first() {
        Some(s) => s,
        None => {
            eprintln!("No grafana service found.");
            return Ok(());
        }
    };

    // Delete instances first
    let instances = sapi
        .list_instances()
        .service_uuid(svc.uuid)
        .send()
        .await
        .context("failed to list grafana instances")?
        .into_inner();

    for inst in &instances {
        eprintln!("Destroying grafana VM {}...", inst.uuid);
        vmapi
            .inner()
            .delete_vm()
            .uuid(inst.uuid)
            .send()
            .await
            .with_context(|| format!("failed to delete VM {}", inst.uuid))?;
        eprintln!("Deleted VM {}.", inst.uuid);

        eprintln!("Deleting SAPI instance {}...", inst.uuid);
        sapi.delete_instance()
            .uuid(inst.uuid)
            .send()
            .await
            .with_context(|| format!("failed to delete SAPI instance {}", inst.uuid))?;
        eprintln!("Deleted SAPI instance {}.", inst.uuid);
    }

    // Delete service
    eprintln!("Deleting grafana service {}...", svc.uuid);
    sapi.delete_service()
        .uuid(svc.uuid)
        .send()
        .await
        .context("failed to delete grafana service")?;
    eprintln!("Deleted grafana service.");

    Ok(())
}
