// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Account info/overview command

use anyhow::Result;
use cloudapi_client::{ClientInfo, TypedClient};

use crate::output::json;

pub async fn run(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let profile_url = client.inner().baseurl();

    // Fetch account details
    let acc_response = client.inner().get_account().account(account).send().await?;
    let acc = acc_response.into_inner();

    // Fetch machines
    let machines_response = client
        .inner()
        .list_machines()
        .account(account)
        .send()
        .await?;
    let machines = machines_response.into_inner();

    // Calculate stats
    let running = machines
        .iter()
        .filter(|m| format!("{:?}", m.state).to_lowercase() == "running")
        .count();
    let total_memory: u64 = machines.iter().map(|m| m.memory).sum();
    let total_disk: u64 = machines.iter().map(|m| m.disk).sum();

    // Build full name from first/last name
    let full_name = match (&acc.first_name, &acc.last_name) {
        (Some(first), Some(last)) => format!("{} {}", first, last),
        (Some(first), None) => first.clone(),
        (None, Some(last)) => last.clone(),
        (None, None) => "-".to_string(),
    };

    if use_json {
        // Match node-triton JSON format
        let info = serde_json::json!({
            "login": acc.login,
            "name": full_name,
            "email": acc.email,
            "url": profile_url,
            "totalDisk": total_disk * 1024 * 1024,  // Convert MB to bytes
            "totalMemory": total_memory * 1024 * 1024,  // Convert MB to bytes
            "instances": machines.len(),
            "running": running,
        });
        json::print_json(&info)?;
    } else {
        // Match node-triton text format
        println!("login: {}", acc.login);
        println!("name: {}", full_name);
        println!("email: {}", acc.email);
        println!("url: {}", profile_url);
        println!("totalDisk: {}", format_gib(total_disk));
        println!("totalMemory: {}", format_gib(total_memory));
        println!("instances: {}", machines.len());
        println!("    running: {}", running);
    }

    Ok(())
}

/// Format MB value as GiB with one decimal place
fn format_gib(mb: u64) -> String {
    let gib = mb as f64 / 1024.0;
    format!("{:.1} GiB", gib)
}
