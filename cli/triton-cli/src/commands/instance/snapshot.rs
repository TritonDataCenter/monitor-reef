// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance snapshot subcommands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use cloudapi_client::types::{Snapshot, SnapshotState};
use dialoguer::Confirm;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Subcommand, Clone)]
pub enum SnapshotCommand {
    /// List snapshots for an instance
    #[command(visible_alias = "ls")]
    List(SnapshotListArgs),

    /// Get snapshot details
    Get(SnapshotGetArgs),

    /// Create a snapshot
    Create(SnapshotCreateArgs),

    /// Delete a snapshot
    #[command(visible_alias = "rm")]
    Delete(SnapshotDeleteArgs),

    /// Boot from a snapshot (rollback)
    Boot(SnapshotBootArgs),
}

#[derive(Args, Clone)]
pub struct SnapshotListArgs {
    /// Instance ID or name
    pub instance: String,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Args, Clone)]
pub struct SnapshotGetArgs {
    /// Instance ID or name
    pub instance: String,

    /// Snapshot name
    pub name: String,
}

#[derive(Args, Clone)]
pub struct SnapshotCreateArgs {
    /// Instance ID or name
    pub instance: String,

    /// Snapshot name
    #[arg(long, short = 'n')]
    pub name: String,

    /// Wait for snapshot creation to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct SnapshotDeleteArgs {
    /// Instance ID or name
    pub instance: String,

    /// Snapshot name
    pub name: String,

    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct SnapshotBootArgs {
    /// Instance ID or name
    pub instance: String,

    /// Snapshot name to boot from
    pub name: String,
}

impl SnapshotCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_snapshots(args, client, use_json).await,
            Self::Get(args) => get_snapshot(args, client, use_json).await,
            Self::Create(args) => create_snapshot(args, client, use_json).await,
            Self::Delete(args) => delete_snapshot(args, client).await,
            Self::Boot(args) => boot_snapshot(args, client).await,
        }
    }
}

pub async fn list_snapshots(
    args: SnapshotListArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let response = client
        .inner()
        .list_machine_snapshots()
        .account(account)
        .machine(machine_id)
        .send()
        .await?;

    let snapshots = response.into_inner();

    if use_json {
        json::print_json_stream(&snapshots)?;
    } else {
        let short_cols = ["name", "state", "created"];
        let long_cols = ["updated"];

        let mut tbl =
            TableBuilder::new(&["NAME", "STATE", "CREATED"]).with_long_headers(&["UPDATED"]);

        let all_cols: Vec<&str> = short_cols.iter().chain(long_cols.iter()).copied().collect();
        for snap in &snapshots {
            let row = all_cols
                .iter()
                .map(|col| get_snapshot_field_value(snap, col))
                .collect();
            tbl.add_row(row);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

fn get_snapshot_field_value(snap: &Snapshot, field: &str) -> String {
    match field.to_lowercase().as_str() {
        "name" => snap.name.clone(),
        "state" => crate::output::enum_to_display(&snap.state),
        "created" => snap.created.to_string(),
        "updated" => snap.updated.clone().unwrap_or_else(|| "-".to_string()),
        _ => "-".to_string(),
    }
}

async fn get_snapshot(args: SnapshotGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let response = client
        .inner()
        .get_machine_snapshot()
        .account(account)
        .machine(machine_id)
        .name(&args.name)
        .send()
        .await?;

    let snapshot = response.into_inner();

    if use_json {
        json::print_json(&snapshot)?;
    } else {
        json::print_json_pretty(&snapshot)?;
    }

    Ok(())
}

async fn create_snapshot(
    args: SnapshotCreateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let request = cloudapi_client::types::CreateSnapshotRequest {
        name: Some(args.name.clone()),
    };

    let response = client
        .inner()
        .create_machine_snapshot()
        .account(account)
        .machine(machine_id)
        .body(request)
        .send()
        .await?;

    let snapshot = response.into_inner();

    println!("Creating snapshot {}", snapshot.name);

    if args.wait {
        let final_snapshot = wait_for_snapshot_state(
            machine_id,
            &snapshot.name,
            SnapshotState::Created,
            args.wait_timeout,
            client,
        )
        .await?;
        println!("Snapshot {} is created", final_snapshot.name);
        if use_json {
            json::print_json(&final_snapshot)?;
        }
    } else if use_json {
        json::print_json(&snapshot)?;
    }

    Ok(())
}

async fn wait_for_snapshot_state(
    machine_id: uuid::Uuid,
    snapshot_name: &str,
    target: SnapshotState,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<cloudapi_client::types::Snapshot> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let response = client
            .inner()
            .get_machine_snapshot()
            .account(account)
            .machine(machine_id)
            .name(snapshot_name)
            .send()
            .await?;

        let snapshot = response.into_inner();

        if snapshot.state == target {
            return Ok(snapshot);
        }

        // Check for failed state
        if snapshot.state == SnapshotState::Failed {
            return Err(anyhow::anyhow!(
                "Snapshot entered failed state while waiting for {}",
                crate::output::enum_to_display(&target),
            ));
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for snapshot to reach state {} (current: {})",
                crate::output::enum_to_display(&target),
                crate::output::enum_to_display(&snapshot.state),
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn delete_snapshot(args: SnapshotDeleteArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt(format!("Delete snapshot {}?", args.name))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    client
        .inner()
        .delete_machine_snapshot()
        .account(account)
        .machine(machine_id)
        .name(&args.name)
        .send()
        .await?;

    println!("Deleted snapshot {}", args.name);

    Ok(())
}

async fn boot_snapshot(args: SnapshotBootArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
    let id_str = machine_id.to_string();

    client
        .inner()
        .start_machine_from_snapshot()
        .account(account)
        .machine(machine_id)
        .name(&args.name)
        .send()
        .await?;

    println!(
        "Booting instance {} from snapshot {}",
        &id_str[..8],
        args.name
    );

    Ok(())
}
