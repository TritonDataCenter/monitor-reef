// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Account info/overview command

use anyhow::Result;
use cloudapi_client::TypedClient;

use crate::output::json;

pub async fn run(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

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
    let stopped = machines
        .iter()
        .filter(|m| format!("{:?}", m.state).to_lowercase() == "stopped")
        .count();
    let total_memory: u64 = machines.iter().map(|m| m.memory).sum();
    let total_disk: u64 = machines.iter().map(|m| m.disk).sum();

    if use_json {
        let info = serde_json::json!({
            "login": acc.login,
            "email": acc.email,
            "instances": {
                "total": machines.len(),
                "running": running,
                "stopped": stopped,
            },
            "memory_used_mb": total_memory,
            "disk_used_mb": total_disk,
        });
        json::print_json(&info)?;
    } else {
        println!("Account: {}", acc.login);
        println!("Email:   {}", acc.email);
        println!();
        println!("Instances:");
        println!("  Total:   {}", machines.len());
        println!("  Running: {}", running);
        println!("  Stopped: {}", stopped);
        println!();
        println!("Resources:");
        println!("  Memory:  {} MB", total_memory);
        println!("  Disk:    {} MB", total_disk);
    }

    Ok(())
}
