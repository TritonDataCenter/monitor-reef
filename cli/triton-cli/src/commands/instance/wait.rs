// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance wait command

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use cloudapi_client::types::{AuditSuccess, Machine, MachineState};
use tokio::time::sleep;

use crate::output::{enum_to_display, json};

#[derive(Args, Clone)]
pub struct WaitArgs {
    /// Instance ID or name
    pub instance: String,

    /// Target state(s) to wait for
    #[arg(long, short, value_enum)]
    pub state: Option<Vec<MachineState>>,

    /// Timeout in seconds
    #[arg(long, default_value = "600")]
    pub timeout: u64,
}

pub async fn run(args: WaitArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let states = args.state.unwrap_or_else(|| vec![MachineState::Running]);

    let machine = wait_for_states(machine_id, &states, args.timeout, client).await?;

    if use_json {
        json::print_json(&machine)?;
    } else {
        let id_str = machine_id.to_string();
        println!(
            "Instance {} is {}",
            &id_str[..8],
            enum_to_display(&machine.state)
        );
    }

    Ok(())
}

pub async fn wait_for_state(
    machine_id: uuid::Uuid,
    target_state: MachineState,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    wait_for_states(machine_id, &[target_state], timeout_secs, client).await?;
    Ok(())
}

pub async fn wait_for_states(
    machine_id: uuid::Uuid,
    target_states: &[MachineState],
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<Machine> {
    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let machine = client.get_machine(account, &machine_id).await?;

        if target_states.contains(&machine.state) {
            return Ok(machine);
        }

        // Check for failed state (unless we're explicitly waiting for it)
        if machine.state == MachineState::Failed && !target_states.contains(&MachineState::Failed) {
            let target_names: Vec<String> = target_states.iter().map(enum_to_display).collect();
            return Err(anyhow::anyhow!(
                "Instance entered failed state while waiting for {}",
                target_names.join(", ")
            ));
        }

        if start.elapsed() > timeout {
            let target_names: Vec<String> = target_states.iter().map(enum_to_display).collect();
            return Err(anyhow::anyhow!(
                "Timeout waiting for instance to reach state {} (current: {})",
                target_names.join(", "),
                enum_to_display(&machine.state)
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

/// Wait for a reboot to complete by polling the audit trail.
///
/// Instead of polling machine state (which is ambiguous — a fast reboot may
/// already show "running" before we first poll), we look for a "reboot" audit
/// entry with a timestamp after `reboot_time` and `success == Yes`.
pub async fn wait_for_reboot(
    machine_id: uuid::Uuid,
    reboot_time: &str,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    // Small initial delay to avoid hitting the API before the action is recorded
    sleep(Duration::from_millis(500)).await;

    loop {
        let audits = client
            .inner()
            .machine_audit()
            .account(account)
            .machine(machine_id)
            .send()
            .await?
            .into_inner();

        // Look for a reboot audit entry newer than our reboot request
        for audit in &audits {
            if audit.action == "reboot" && audit.time.as_str() > reboot_time {
                if audit.success == Some(AuditSuccess::Yes) {
                    return Ok(());
                } else {
                    return Err(anyhow::anyhow!(
                        "Reboot failed (audit success={})",
                        audit
                            .success
                            .as_ref()
                            .map(enum_to_display)
                            .unwrap_or_else(|| "unknown".to_string())
                    ));
                }
            }
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for reboot to complete"));
        }

        sleep(Duration::from_secs(2)).await;
    }
}
