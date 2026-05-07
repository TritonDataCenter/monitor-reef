// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonadm channel` — manage the active update channel.
//!
//! The channel is stored as `update_channel` in the SAPI `sdc`
//! application's metadata. When unset, tritonadm and other tooling
//! fall back to whichever channel the updates server marks as default
//! (see `post_setup::resolve_channel`).

use anyhow::{Context, Result};
use clap::Subcommand;
use sapi_client::types;

use crate::DEFAULT_UPDATES_URL;

#[derive(Subcommand)]
pub enum ChannelCommand {
    /// List available update channels
    List {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
    },
    /// Set the current update channel
    Set {
        /// Channel name
        name: String,
    },
    /// Unset the current update channel (fall back to remote default)
    Unset,
    /// Get the current update channel
    Get,
}

impl ChannelCommand {
    pub async fn run(self, sapi_url: &str, updates_url: Option<&str>) -> Result<()> {
        match self {
            Self::List { json } => cmd_list(sapi_url, updates_url, json).await,
            Self::Set { name } => cmd_set(sapi_url, updates_url, &name).await,
            Self::Unset => cmd_unset(sapi_url).await,
            Self::Get => cmd_get(sapi_url, updates_url).await,
        }
    }
}

/// Build an updates-server IMGAPI client. The updates server URL comes
/// from the `--updates-url` flag / `UPDATES_URL` env var (resolved at
/// the CLI layer); when not set, fall back to the canonical Triton
/// updates host so `tritonadm channel list` works on a fresh headnode
/// before anyone's set up SAPI metadata.
async fn updates_client(updates_url: Option<&str>) -> Result<imgapi_client::Client> {
    let url = updates_url.unwrap_or(DEFAULT_UPDATES_URL);
    let http = triton_tls::build_http_client(false)
        .await
        .map_err(|e| anyhow::anyhow!("build http client: {e}"))?;
    Ok(imgapi_client::Client::new_with_client(url, http))
}

async fn fetch_sdc_app(sapi: &sapi_client::Client) -> Result<types::Application> {
    let apps = sapi
        .list_applications()
        .name("sdc")
        .send()
        .await
        .context("failed to list applications")?
        .into_inner();
    apps.into_iter()
        .next()
        .context("no 'sdc' application found in SAPI")
}

/// Read the configured channel from the sdc app's metadata. Returns
/// `None` when unset.
fn configured_channel(app: &types::Application) -> Option<&str> {
    app.metadata
        .as_ref()?
        .get("update_channel")?
        .as_str()
        .filter(|s| !s.is_empty())
}

async fn cmd_list(sapi_url: &str, updates_url: Option<&str>, json: bool) -> Result<()> {
    let sapi = sapi_client::build_client(sapi_url, false)
        .await
        .context("failed to build SAPI client")?;
    let updates = updates_client(updates_url).await?;

    let channels = updates
        .list_channels()
        .send()
        .await
        .context("failed to list channels from updates server")?
        .into_inner();
    let app = fetch_sdc_app(&sapi).await?;
    let configured = configured_channel(&app);

    if json {
        // Mirror sdcadm's JSON shape: configured channel becomes the
        // `default: true` row; the updates server's own default is
        // dropped (sdcadm's view is "what does this DC use").
        let rows: Vec<serde_json::Value> = channels
            .iter()
            .map(|c| {
                let is_default = match configured {
                    Some(name) => name == c.name,
                    None => c.default == Some(true),
                };
                let mut obj = serde_json::json!({
                    "name": c.name,
                    "description": c.description,
                });
                if is_default {
                    obj["default"] = serde_json::Value::Bool(true);
                    if configured.is_none() {
                        obj["remote"] = serde_json::Value::Bool(true);
                    }
                }
                obj
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    // Plain table. Width-pad NAME and DEFAULT for legibility; description
    // can be long so let it run to EOL.
    println!("{:<16} {:<14} DESCRIPTION", "NAME", "DEFAULT");
    for c in &channels {
        let default_label = match configured {
            Some(name) if name == c.name => "true".to_string(),
            Some(_) => String::new(),
            None if c.default == Some(true) => "true (remote)".to_string(),
            None => String::new(),
        };
        println!("{:<16} {:<14} {}", c.name, default_label, c.description);
    }
    Ok(())
}

async fn cmd_get(sapi_url: &str, updates_url: Option<&str>) -> Result<()> {
    let sapi = sapi_client::build_client(sapi_url, false)
        .await
        .context("failed to build SAPI client")?;
    let app = fetch_sdc_app(&sapi).await?;

    if let Some(ch) = configured_channel(&app) {
        println!("{ch}");
        return Ok(());
    }

    // Fall back to the updates server's default — same priority chain
    // as `post_setup::resolve_channel` minus the explicit-flag arm.
    let updates = updates_client(updates_url).await?;
    let channels = updates
        .list_channels()
        .send()
        .await
        .context("failed to query updates server for default channel")?
        .into_inner();
    let default = channels
        .iter()
        .find(|c| c.default == Some(true))
        .or_else(|| channels.first())
        .context("updates server has no channels configured")?;
    println!("{} (remote default)", default.name);
    Ok(())
}

async fn cmd_set(sapi_url: &str, updates_url: Option<&str>, name: &str) -> Result<()> {
    let sapi = sapi_client::build_client(sapi_url, false)
        .await
        .context("failed to build SAPI client")?;
    let updates = updates_client(updates_url).await?;

    // Validate against the live channel list. sdcadm rejects unknown
    // channels for the same reason: a typo here is hard to notice and
    // produces silently-wrong behavior on the next update query.
    let channels = updates
        .list_channels()
        .send()
        .await
        .context("failed to list channels from updates server")?
        .into_inner();
    if !channels.iter().any(|c| c.name == name) {
        let valid: Vec<&str> = channels.iter().map(|c| c.name.as_str()).collect();
        anyhow::bail!(
            "{name:?} is not a valid channel; try one of: {}",
            valid.join(", ")
        );
    }

    let app = fetch_sdc_app(&sapi).await?;
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "update_channel".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    sapi.update_application()
        .uuid(app.uuid)
        .body(types::UpdateApplicationBody {
            action: Some(types::UpdateAction::Update),
            metadata: Some(metadata),
            ..Default::default()
        })
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("update sdc application metadata: {e}"))?;

    println!("Update channel set to {name:?}.");
    Ok(())
}

async fn cmd_unset(sapi_url: &str) -> Result<()> {
    let sapi = sapi_client::build_client(sapi_url, false)
        .await
        .context("failed to build SAPI client")?;
    let app = fetch_sdc_app(&sapi).await?;

    if configured_channel(&app).is_none() {
        println!("No update channel was set.");
        return Ok(());
    }

    // SAPI's delete action keys off the metadata map; the value is
    // ignored. Pass the current value for clarity in the audit trail.
    let current = configured_channel(&app).unwrap_or("").to_string();
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "update_channel".to_string(),
        serde_json::Value::String(current),
    );
    sapi.update_application()
        .uuid(app.uuid)
        .body(types::UpdateApplicationBody {
            action: Some(types::UpdateAction::Delete),
            metadata: Some(metadata),
            ..Default::default()
        })
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("delete sdc.update_channel metadata: {e}"))?;

    println!("Update channel unset (will fall back to remote default).");
    Ok(())
}
