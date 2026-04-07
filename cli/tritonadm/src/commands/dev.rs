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
#[allow(clippy::enum_variant_names)]
pub enum DevCommand {
    /// Remove external NICs from imgapi and adminui (undo common-external-nics)
    RemoveExternalNics,
    /// Remove CloudAPI instances (undo post-setup cloudapi, keeps service)
    RemoveCloudapi,
    /// Remove grafana service and instance (undo post-setup grafana)
    RemoveGrafana,
    /// Remove portal service and instance (undo post-setup portal)
    RemovePortal,
    /// Remove a named service and its instances
    RemoveService {
        /// Service name to remove
        name: String,
    },
}

impl DevCommand {
    pub async fn run(self, sapi_url: &str, vmapi_url: &str, napi_url: &str) -> Result<()> {
        match self {
            Self::RemoveExternalNics => {
                cmd_remove_external_nics(sapi_url, vmapi_url, napi_url).await
            }
            Self::RemoveCloudapi => {
                cmd_remove_instances_only(sapi_url, vmapi_url, "cloudapi").await
            }
            Self::RemoveGrafana => cmd_remove_service(sapi_url, vmapi_url, "grafana").await,
            Self::RemovePortal => cmd_remove_service(sapi_url, vmapi_url, "portal").await,
            Self::RemoveService { name } => cmd_remove_service(sapi_url, vmapi_url, &name).await,
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

/// Remove all instances of a service without deleting the service itself.
///
/// Used for core services (like CloudAPI) whose SAPI service definition is
/// created by headnode setup and should not be deleted.
async fn cmd_remove_instances_only(sapi_url: &str, vmapi_url: &str, svc_name: &str) -> Result<()> {
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;

    let sapi = sapi_client::Client::new_with_client(sapi_url, http.clone());
    let vmapi = vmapi_client::TypedClient::new_with_client(vmapi_url, http);

    let instances = get_service_instances(&sapi, svc_name).await?;

    if instances.is_empty() {
        eprintln!("No {svc_name} instances found.");
        return Ok(());
    }

    for inst in &instances {
        eprintln!("Destroying {svc_name} VM {}...", inst.uuid);
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

    eprintln!(
        "Removed {} {svc_name} instance(s). Service definition kept.",
        instances.len()
    );
    Ok(())
}

async fn cmd_remove_service(sapi_url: &str, vmapi_url: &str, svc_name: &str) -> Result<()> {
    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;

    let sapi = sapi_client::Client::new_with_client(sapi_url, http.clone());
    let vmapi = vmapi_client::TypedClient::new_with_client(vmapi_url, http);

    let services = sapi
        .list_services()
        .name(svc_name)
        .send()
        .await
        .with_context(|| format!("failed to list {svc_name} services"))?
        .into_inner();

    let svc = match services.first() {
        Some(s) => s,
        None => {
            eprintln!("No {svc_name} service found.");
            return Ok(());
        }
    };

    let instances = sapi
        .list_instances()
        .service_uuid(svc.uuid)
        .send()
        .await
        .with_context(|| format!("failed to list {svc_name} instances"))?
        .into_inner();

    for inst in &instances {
        eprintln!("Destroying {svc_name} VM {}...", inst.uuid);
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

    eprintln!("Deleting {svc_name} service {}...", svc.uuid);
    sapi.delete_service()
        .uuid(svc.uuid)
        .send()
        .await
        .with_context(|| format!("failed to delete {svc_name} service"))?;
    eprintln!("Deleted {svc_name} service.");

    Ok(())
}
