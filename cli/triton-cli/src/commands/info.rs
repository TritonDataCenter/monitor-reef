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
        // Build instances object with state counts
        let mut states: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for m in &machines {
            let state = format!("{:?}", m.state).to_lowercase();
            *states.entry(state).or_insert(0) += 1;
        }

        let info = serde_json::json!({
            "login": acc.login,
            "name": full_name,
            "email": acc.email,
            "url": profile_url,
            "totalDisk": total_disk * 1000 * 1000,  // Convert MB to bytes (decimal)
            "totalMemory": total_memory * 1000 * 1000,  // Convert MB to bytes (decimal)
            "totalInstances": machines.len(),
            "instances": states,
        });
        json::print_json(&info)?;
    } else {
        // Match node-triton text format
        // Build instances object with state counts
        let mut states: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for m in &machines {
            let state = format!("{:?}", m.state).to_lowercase();
            *states.entry(state).or_insert(0) += 1;
        }

        println!("login: {}", acc.login);
        println!("name: {}", full_name);
        println!("email: {}", acc.email);
        println!("url: {}", profile_url);
        // node-triton converts to bytes with *1000*1000 then displays with humanSizeFromBytes
        println!("totalDisk: {}", human_size_from_bytes(total_disk * 1000 * 1000));
        println!("totalMemory: {}", human_size_from_bytes(total_memory * 1000 * 1000));
        println!("instances: {}", machines.len());
        for (state, count) in &states {
            println!("    {}: {}", state, count);
        }
    }

    Ok(())
}

/// Format bytes as human-readable size (matches node-triton humanSizeFromBytes)
fn human_size_from_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }

    const SIZES: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let i = (bytes as f64).log(1024.0).floor() as usize;
    let size = bytes as f64 / 1024_f64.powi(i as i32);
    format!("{:.1} {}", size, SIZES[i])
}
