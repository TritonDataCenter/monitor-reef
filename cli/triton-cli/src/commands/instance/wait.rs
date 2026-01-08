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
use cloudapi_client::types::Machine;
use tokio::time::sleep;

use crate::output::json;

#[derive(Args, Clone)]
pub struct WaitArgs {
    /// Instance ID or name
    pub instance: String,

    /// Target state(s) to wait for
    #[arg(long, short)]
    pub state: Option<Vec<String>>,

    /// Timeout in seconds
    #[arg(long, default_value = "600")]
    pub timeout: u64,
}

pub async fn run(args: WaitArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let states = args.state.unwrap_or_else(|| vec!["running".to_string()]);

    let machine = wait_for_states(machine_id, &states, args.timeout, client).await?;

    if use_json {
        json::print_json(&machine)?;
    } else {
        let id_str = machine_id.to_string();
        println!("Instance {} is {:?}", &id_str[..8], machine.state);
    }

    Ok(())
}

pub async fn wait_for_state(
    machine_id: uuid::Uuid,
    target_state: &str,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    wait_for_states(
        machine_id,
        &[target_state.to_string()],
        timeout_secs,
        client,
    )
    .await?;
    Ok(())
}

pub async fn wait_for_states(
    machine_id: uuid::Uuid,
    target_states: &[String],
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<Machine> {
    let account = &client.auth_config().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let response = client
            .inner()
            .get_machine()
            .account(account)
            .machine(machine_id)
            .send()
            .await?;

        let machine = response.into_inner();
        let current_state = format!("{:?}", machine.state).to_lowercase();

        if target_states
            .iter()
            .any(|s| s.to_lowercase() == current_state)
        {
            return Ok(machine);
        }

        // Check for failed state
        if current_state == "failed" {
            return Err(anyhow::anyhow!(
                "Instance entered failed state while waiting for {:?}",
                target_states
            ));
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for instance to reach state {:?} (current: {})",
                target_states,
                current_state
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}
