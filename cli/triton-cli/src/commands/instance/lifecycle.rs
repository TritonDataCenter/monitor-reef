// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance lifecycle commands (start, stop, reboot)

use anyhow::Result;
use clap::Args;
use cloudapi_client::types::MachineState;

use crate::client::AnyClient;
use crate::dispatch;

#[derive(Args, Clone)]
pub struct StartArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,

    /// Boot from a snapshot
    #[arg(long)]
    pub snapshot: Option<String>,

    /// Wait for instance to be running
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct StopArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,

    /// Wait for instance to be stopped
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct RebootArgs {
    /// Instance ID(s) or name(s)
    pub instances: Vec<String>,

    /// Wait for instance to be running
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

/// Body for the cloudapi action-dispatch endpoint.
///
/// Mirrors `cloudapi_client::ActionBody` but defined locally so the
/// dispatch arm can hand an opaque `serde_json::Value` to either generated
/// client's `update_machine().body(...)` builder.
fn start_body(origin: Option<&str>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("action".into(), serde_json::Value::String("start".into()));
    if let Some(o) = origin {
        obj.insert("origin".into(), serde_json::Value::String(o.into()));
    }
    serde_json::Value::Object(obj)
}

fn stop_body(origin: Option<&str>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("action".into(), serde_json::Value::String("stop".into()));
    if let Some(o) = origin {
        obj.insert("origin".into(), serde_json::Value::String(o.into()));
    }
    serde_json::Value::Object(obj)
}

fn reboot_body(origin: Option<&str>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("action".into(), serde_json::Value::String("reboot".into()));
    if let Some(o) = origin {
        obj.insert("origin".into(), serde_json::Value::String(o.into()));
    }
    serde_json::Value::Object(obj)
}

pub async fn start(args: StartArgs, client: &AnyClient) -> Result<()> {
    let total = args.instances.len();
    let mut errors = Vec::new();

    for instance in &args.instances {
        let machine_id = match super::get::resolve_instance(instance, client).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Error: {}: {}", instance, e);
                errors.push(format!("{}: {}", instance, e));
                continue;
            }
        };
        let account = client.effective_account();
        let id_str = machine_id.to_string();

        let start_result: anyhow::Result<()> = if let Some(ref snap) = args.snapshot {
            dispatch!(client, |c| {
                c.inner()
                    .start_machine_from_snapshot()
                    .account(account)
                    .machine(machine_id)
                    .name(snap)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            })
        } else {
            let body = start_body(None);
            dispatch!(client, |c| {
                c.inner()
                    .update_machine()
                    .account(account)
                    .machine(machine_id)
                    .body(body)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            })
        };

        if let Err(e) = start_result {
            #[cfg(debug_assertions)]
            if e.to_string()
                .contains(cloudapi_client::EMIT_PAYLOAD_SENTINEL)
            {
                continue;
            }
            eprintln!("Error starting {}: {}", &id_str[..8], e);
            errors.push(format!("{}: {}", &id_str[..8], e));
            continue;
        }

        if args.wait {
            match super::wait::wait_for_state(
                machine_id,
                MachineState::Running,
                args.wait_timeout,
                client,
            )
            .await
            {
                Ok(()) => println!("Start instance {}", &id_str[..8]),
                Err(e) => {
                    eprintln!("Error waiting for {}: {}", &id_str[..8], e);
                    errors.push(format!("{}: {}", &id_str[..8], e));
                }
            }
        } else {
            println!("Start (async) instance {}", &id_str[..8]);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} of {} instances failed",
            errors.len(),
            total
        ))
    }
}

pub async fn stop(args: StopArgs, client: &AnyClient) -> Result<()> {
    let total = args.instances.len();
    let mut errors = Vec::new();

    for instance in &args.instances {
        let machine_id = match super::get::resolve_instance(instance, client).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Error: {}: {}", instance, e);
                errors.push(format!("{}: {}", instance, e));
                continue;
            }
        };
        let account = client.effective_account();
        let id_str = machine_id.to_string();

        let body = stop_body(None);
        let result: anyhow::Result<()> = dispatch!(client, |c| {
            c.inner()
                .update_machine()
                .account(account)
                .machine(machine_id)
                .body(body)
                .send()
                .await
                .map(|_| ())
                .map_err(anyhow::Error::from)
        });

        if let Err(e) = result {
            #[cfg(debug_assertions)]
            if e.to_string()
                .contains(cloudapi_client::EMIT_PAYLOAD_SENTINEL)
            {
                continue;
            }
            eprintln!("Error stopping {}: {}", &id_str[..8], e);
            errors.push(format!("{}: {}", &id_str[..8], e));
            continue;
        }

        if args.wait {
            match super::wait::wait_for_state(
                machine_id,
                MachineState::Stopped,
                args.wait_timeout,
                client,
            )
            .await
            {
                Ok(()) => println!("Stop instance {}", &id_str[..8]),
                Err(e) => {
                    eprintln!("Error waiting for {}: {}", &id_str[..8], e);
                    errors.push(format!("{}: {}", &id_str[..8], e));
                }
            }
        } else {
            println!("Stop (async) instance {}", &id_str[..8]);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} of {} instances failed",
            errors.len(),
            total
        ))
    }
}

pub async fn reboot(args: RebootArgs, client: &AnyClient) -> Result<()> {
    let total = args.instances.len();
    let mut errors = Vec::new();

    for instance in &args.instances {
        let machine_id = match super::get::resolve_instance(instance, client).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Error: {}: {}", instance, e);
                errors.push(format!("{}: {}", instance, e));
                continue;
            }
        };
        let account = client.effective_account();
        let id_str = machine_id.to_string();

        // Record the time before issuing the reboot so we can find the
        // corresponding audit entry later.
        let reboot_time = chrono::Utc::now().to_rfc3339();

        let body = reboot_body(None);
        let result: anyhow::Result<()> = dispatch!(client, |c| {
            c.inner()
                .update_machine()
                .account(account)
                .machine(machine_id)
                .body(body)
                .send()
                .await
                .map(|_| ())
                .map_err(anyhow::Error::from)
        });

        if let Err(e) = result {
            #[cfg(debug_assertions)]
            if e.to_string()
                .contains(cloudapi_client::EMIT_PAYLOAD_SENTINEL)
            {
                continue;
            }
            eprintln!("Error rebooting {}: {}", &id_str[..8], e);
            errors.push(format!("{}: {}", &id_str[..8], e));
            continue;
        }

        println!("Rebooting instance {}", &id_str[..8]);

        if args.wait {
            // Use audit-trail polling instead of state polling. Polling for
            // state==running is ambiguous because a fast reboot (or one that
            // hasn't started yet) may already show "running".
            match super::wait::wait_for_reboot(machine_id, &reboot_time, args.wait_timeout, client)
                .await
            {
                Ok(()) => println!("Rebooted instance {}", &id_str[..8]),
                Err(e) => {
                    eprintln!("Error waiting for {}: {}", &id_str[..8], e);
                    errors.push(format!("{}: {}", &id_str[..8], e));
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} of {} instances failed",
            errors.len(),
            total
        ))
    }
}
