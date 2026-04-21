// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Account info/overview command

use anyhow::Result;
use std::collections::HashMap;

use crate::client::AnyClient;
use crate::dispatch;
use crate::output::{enum_to_display, json};

pub async fn run(client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let profile_url = client.baseurl().to_string();

    // Push the field extraction inside the dispatch so only std-typed
    // values escape each arm. The per-crate Progenitor types (`Account`,
    // `Machine`) stay local to the match arm, and the tuple we return is
    // uniform across variants.
    let (login, full_name, email, machine_count, total_memory, total_disk, states) =
        dispatch!(client, |c| {
            let acc = c
                .inner()
                .get_account()
                .account(account)
                .send()
                .await?
                .into_inner();
            let machines = c
                .inner()
                .list_machines()
                .account(account)
                .send()
                .await?
                .into_inner();

            let total_memory: u64 = machines.iter().filter_map(|m| m.memory).sum();
            let total_disk: u64 = machines.iter().map(|m| m.disk).sum();
            let full_name = match (&acc.first_name, &acc.last_name) {
                (Some(first), Some(last)) => format!("{} {}", first, last),
                (Some(first), None) => first.clone(),
                (None, Some(last)) => last.clone(),
                (None, None) => "-".to_string(),
            };
            let mut states: HashMap<String, usize> = HashMap::new();
            for m in &machines {
                let state = enum_to_display(&m.state);
                *states.entry(state).or_insert(0) += 1;
            }

            (
                acc.login,
                full_name,
                acc.email,
                machines.len(),
                total_memory,
                total_disk,
                states,
            )
        });

    if use_json {
        let info = serde_json::json!({
            "login": login,
            "name": full_name,
            "email": email,
            "url": profile_url,
            // Decimal MB→bytes matches node-triton do_info.js:71-72
            "totalDisk": total_disk * 1000 * 1000,
            "totalMemory": total_memory * 1000 * 1000,
            "totalInstances": machine_count,
            "instances": states,
        });
        json::print_json(&info)?;
    } else {
        println!("login: {}", login);
        println!("name: {}", full_name);
        println!("email: {}", email);
        println!("url: {}", profile_url);
        println!(
            "totalDisk: {}",
            human_size_from_bytes(total_disk * 1000 * 1000)
        );
        println!(
            "totalMemory: {}",
            human_size_from_bytes(total_memory * 1000 * 1000)
        );
        println!("instances: {}", machine_count);
        for (state, count) in &states {
            println!("    {}: {}", state, count);
        }
    }

    Ok(())
}

/// Format bytes as human-readable size
/// Matches node-triton common.js:355-407 humanSizeFromBytes (default/non-narrow mode)
fn human_size_from_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }

    const SIZES: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let i = (bytes as f64).log(1024.0).floor() as usize;
    let i = i.min(SIZES.len() - 1);
    let size = bytes as f64 / 1024_f64.powi(i as i32);
    // Truncate (not round) to 1 decimal place, matching Node.js behavior
    let truncated = (size * 10.0).floor() / 10.0;
    format!("{:.1} {}", truncated, SIZES[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size_from_bytes_truncates() {
        // 102400000000 bytes = 95.367... GiB → truncated to 95.3
        assert_eq!(human_size_from_bytes(102_400_000_000), "95.3 GiB");
        // 8192000000 bytes = 7.629... GiB → truncated to 7.6
        assert_eq!(human_size_from_bytes(8_192_000_000), "7.6 GiB");
        assert_eq!(human_size_from_bytes(0), "0 B");
        assert_eq!(human_size_from_bytes(1024), "1.0 KiB");
    }
}
