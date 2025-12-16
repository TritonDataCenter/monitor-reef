// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance VNC command
//!
//! Provides VNC console access information for bhyve/KVM instances.

use anyhow::Result;
use clap::Args;
use cloudapi_client::{ClientInfo, TypedClient};

use super::get::resolve_instance;

#[derive(Args, Clone)]
pub struct VncArgs {
    /// Instance name, short ID, or UUID
    pub instance: String,
}

/// VNC connection info (returned for JSON output)
#[derive(serde::Serialize)]
pub struct VncInfo {
    pub instance: String,
    pub url: String,
}

pub async fn run(args: VncArgs, client: &TypedClient, json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let machine_id = resolve_instance(&args.instance, client).await?;

    // Get the base URL from the client to construct the WebSocket URL
    // The VNC endpoint is a WebSocket endpoint at /{account}/machines/{machine}/vnc
    let base_url = client.inner().baseurl();

    // Convert http(s) to ws(s)
    let ws_url = base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");

    let vnc_url = format!("{}/{}/machines/{}/vnc", ws_url, account, machine_id);

    if json {
        let info = VncInfo {
            instance: machine_id,
            url: vnc_url,
        };
        crate::output::json::print_json(&info)?;
    } else {
        println!("VNC WebSocket URL: {}", vnc_url);
        println!();
        println!("To connect, use a VNC client that supports WebSocket connections,");
        println!("or use a noVNC client with this URL.");
        println!();
        println!("Note: VNC access is only available for running bhyve/KVM instances.");
    }

    Ok(())
}
