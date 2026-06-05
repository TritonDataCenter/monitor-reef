// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::not_yet_implemented;

#[derive(Subcommand)]
pub enum DcMaintCommand {
    /// Start datacenter maintenance
    Start,
    /// Stop datacenter maintenance
    Stop,
    /// Show datacenter maintenance status
    Status {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
    },
}

impl DcMaintCommand {
    pub async fn run(self, sapi_url: &str) -> Result<()> {
        match self {
            Self::Start => not_yet_implemented("dc-maint start"),
            Self::Stop => not_yet_implemented("dc-maint stop"),
            Self::Status { json } => cmd_dc_maint_status(sapi_url, json).await,
        }
    }
}

async fn cmd_dc_maint_status(sapi_url: &str, json: bool) -> Result<()> {
    let sapi = sapi_client::build_client(sapi_url, false)
        .await
        .context("failed to build SAPI client")?;

    // Get the "sdc" application to read DC_MAINT_MESSAGE and DC_MAINT_ETA
    let apps = sapi
        .list_applications()
        .name("sdc")
        .send()
        .await
        .context("failed to list applications")?
        .into_inner();
    let sdc_app = apps.first().context("no 'sdc' application found in SAPI")?;

    // Check cloudapi service for CLOUDAPI_READONLY. `list_services(name=...)`
    // returns an empty list (not 404) when the service isn't deployed, so any
    // error here is a genuine SAPI failure — we must not silently report
    // "off" because we couldn't read the state.
    let cloudapi_maint = sapi
        .list_services()
        .name("cloudapi")
        .send()
        .await
        .context("failed to read cloudapi service from SAPI")?
        .into_inner()
        .first()
        .and_then(|svc| svc.metadata.as_ref()?.get("CLOUDAPI_READONLY")?.as_bool())
        .unwrap_or(false);

    // Check docker service for DOCKER_READONLY (same rationale as above).
    let docker_maint = sapi
        .list_services()
        .name("docker")
        .send()
        .await
        .context("failed to read docker service from SAPI")?
        .into_inner()
        .first()
        .and_then(|svc| svc.metadata.as_ref()?.get("DOCKER_READONLY")?.as_bool())
        .unwrap_or(false);

    let maint = cloudapi_maint || docker_maint;

    let message = sdc_app
        .metadata
        .as_ref()
        .and_then(|m| m.get("DC_MAINT_MESSAGE"))
        .and_then(|v| v.as_str());
    let eta = sdc_app
        .metadata
        .as_ref()
        .and_then(|m| m.get("DC_MAINT_ETA"))
        .and_then(|v| v.as_str());

    if json {
        let status = serde_json::json!({
            "maint": maint,
            "cloudapiMaint": cloudapi_maint,
            "dockerMaint": docker_maint,
            "message": message,
            "eta": eta,
        });
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else if maint {
        let mode = if cloudapi_maint && docker_maint {
            "on"
        } else if cloudapi_maint {
            "cloudapi-only"
        } else {
            "docker-only"
        };
        println!("DC maintenance: {mode}");
        if let Some(msg) = message {
            println!("DC maintenance message: {msg}");
        }
        if let Some(eta_val) = eta {
            println!("DC maintenance ETA: {eta_val}");
        }
    } else {
        println!("DC maintenance: off");
    }
    Ok(())
}
