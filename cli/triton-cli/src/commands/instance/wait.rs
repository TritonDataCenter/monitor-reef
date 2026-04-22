// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance wait command

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args;
// Keep the per-client (Progenitor-generated) `MachineState` / `AuditSuccess`
// enums here because Progenitor patches them with `clap::ValueEnum` — the
// canonical `cloudapi_api::MachineState` is not ValueEnum-capable, so the
// `wait --state` CLI argument needs the generated variant.
use cloudapi_client::types::{AuditSuccess, MachineState};
use tokio::time::sleep;

use crate::client::AnyClient;
use crate::dispatch;
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

pub async fn run(args: WaitArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let states = args.state.unwrap_or_else(|| vec![MachineState::Running]);

    let (machine_state, machine_json) =
        wait_for_states(machine_id, &states, args.timeout, client).await?;

    if use_json {
        json::print_json(&machine_json)?;
    } else {
        let id_str = machine_id.to_string();
        println!(
            "Instance {} is {}",
            &id_str[..8],
            enum_to_display(&machine_state)
        );
    }

    Ok(())
}

pub async fn wait_for_state(
    machine_id: uuid::Uuid,
    target_state: MachineState,
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<()> {
    wait_for_states(machine_id, &[target_state], timeout_secs, client).await?;
    Ok(())
}

/// Poll the gateway/cloudapi for the machine's current state. Returns the
/// canonical `(state, serialized-body)` tuple so callers can either act on
/// the enum or emit JSON without dealing with per-client Progenitor types.
pub async fn wait_for_states(
    machine_id: uuid::Uuid,
    target_states: &[MachineState],
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<(MachineState, serde_json::Value)> {
    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let (state, machine_json): (MachineState, serde_json::Value) = dispatch!(client, |c| {
            let resp = c
                .inner()
                .get_machine()
                .account(account)
                .machine(machine_id)
                .send()
                .await?
                .into_inner();
            let value = serde_json::to_value(&resp)?;
            // Re-deserialize the `state` field to the canonical
            // `cloudapi_api::MachineState`. Both per-client enums serialize to
            // the same wire string, so a JSON round-trip is exact.
            let state: MachineState = serde_json::from_value(
                value
                    .get("state")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )?;
            (state, value)
        });

        if target_states.contains(&state) {
            return Ok((state, machine_json));
        }

        // Check for failed state (unless we're explicitly waiting for it)
        if state == MachineState::Failed && !target_states.contains(&MachineState::Failed) {
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
                enum_to_display(&state)
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
    client: &AnyClient,
) -> Result<()> {
    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    // Small initial delay to avoid hitting the API before the action is recorded
    sleep(Duration::from_millis(500)).await;

    loop {
        // Pull audit entries as a stable `Vec<(action, time, success)>`
        // tuple list so the per-client `AuditEntry` type stays inside the
        // dispatch arm.
        let audits: Vec<(String, String, Option<AuditSuccess>)> = dispatch!(client, |c| {
            let resp = c
                .inner()
                .machine_audit()
                .account(account)
                .machine(machine_id)
                .send()
                .await?
                .into_inner();
            resp.into_iter()
                .map(|a| {
                    // Round-trip the per-client `AuditSuccess` to the
                    // canonical `cloudapi_api::AuditSuccess`; both enums
                    // serialize to the same `"yes"` / `"no"` wire values.
                    let success: Option<AuditSuccess> = a
                        .success
                        .as_ref()
                        .and_then(|s| serde_json::to_value(s).ok())
                        .and_then(|v| serde_json::from_value(v).ok());
                    Ok::<_, anyhow::Error>((a.action, a.time, success))
                })
                .collect::<Result<Vec<_>, _>>()?
        });

        // Look for a reboot audit entry newer than our reboot request
        for (action, time, success) in &audits {
            if action == "reboot" && time.as_str() > reboot_time {
                if *success == Some(AuditSuccess::Yes) {
                    return Ok(());
                } else {
                    return Err(anyhow::anyhow!(
                        "Reboot failed (audit success={})",
                        success
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
