// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance snapshot subcommands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_api::{Snapshot, SnapshotState};
use dialoguer::Confirm;

use crate::client::AnyClient;
use crate::define_columns;
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};
use crate::{dispatch, dispatch_with_types};

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

    /// Wait for snapshot deletion to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct SnapshotBootArgs {
    /// Instance ID or name
    pub instance: String,

    /// Snapshot name to boot from
    pub name: String,
}

impl SnapshotCommand {
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
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
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let mut snapshots: Vec<Snapshot> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_machine_snapshots()
            .account(account)
            .machine(machine_id)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Vec<Snapshot>>(serde_json::to_value(&resp)?)?
    });
    snapshots.sort_by(|a, b| a.name.cmp(&b.name));

    if use_json {
        json::print_json_stream(&snapshots)?;
    } else {
        define_columns! {
            SnapshotColumn for Snapshot, long_from: 3, {
                Name("NAME") => |snap| snap.name.clone(),
                State("STATE") => |snap| crate::output::enum_to_display(&snap.state),
                Created("CREATED") => |snap| snap.created.as_deref().unwrap_or("-").to_string(),
                // --- long-only columns below ---
                Updated("UPDATED") => |snap| {
                    snap.updated.clone().unwrap_or_else(|| "-".to_string())
                },
            }
        }

        TableBuilder::from_enum_columns::<SnapshotColumn, _>(
            &snapshots,
            Some(SnapshotColumn::LONG_FROM),
        )
        .print(&args.table)?;
    }

    Ok(())
}

async fn get_snapshot(args: SnapshotGetArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let snapshot_json: serde_json::Value = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_machine_snapshot()
            .account(account)
            .machine(machine_id)
            .name(&args.name)
            .send()
            .await?
            .into_inner();
        serde_json::to_value(&resp)?
    });

    if use_json {
        json::print_json(&snapshot_json)?;
    } else {
        json::print_json_pretty(&snapshot_json)?;
    }

    Ok(())
}

async fn create_snapshot(
    args: SnapshotCreateArgs,
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let snapshot_name = args.name.clone();
    let snapshot: Snapshot = dispatch_with_types!(client, |c, t| {
        let body = t::CreateSnapshotRequest {
            name: Some(snapshot_name.clone()),
        };
        let resp = c
            .inner()
            .create_machine_snapshot()
            .account(account)
            .machine(machine_id)
            .body(body)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Snapshot>(serde_json::to_value(&resp)?)?
    });

    eprintln!("Creating snapshot {}", snapshot.name);

    if args.wait {
        let final_snapshot = wait_for_snapshot_state(
            machine_id,
            &snapshot.name,
            SnapshotState::Created,
            args.wait_timeout,
            client,
        )
        .await?;
        println!("Created snapshot \"{}\"", final_snapshot.name);
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
    client: &AnyClient,
) -> Result<Snapshot> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let snapshot: Snapshot = dispatch!(client, |c| {
            let resp = c
                .inner()
                .get_machine_snapshot()
                .account(account)
                .machine(machine_id)
                .name(snapshot_name)
                .send()
                .await?
                .into_inner();
            serde_json::from_value::<Snapshot>(serde_json::to_value(&resp)?)?
        });

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

async fn wait_for_snapshot_deleted(
    machine_id: uuid::Uuid,
    snapshot_name: &str,
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        // A successful fetch means the snapshot still exists; a 404 on
        // send() means it was deleted. We collapse both signals into a
        // `Option<SnapshotState>` the dispatch arm returns.
        let state: Option<SnapshotState> = dispatch!(client, |c| {
            match c
                .inner()
                .get_machine_snapshot()
                .account(account)
                .machine(machine_id)
                .name(snapshot_name)
                .send()
                .await
            {
                Err(_) => None,
                Ok(resp) => {
                    let snap: Snapshot = serde_json::from_value::<Snapshot>(serde_json::to_value(
                        &resp.into_inner(),
                    )?)?;
                    Some(snap.state)
                }
            }
        });

        match state {
            None => return Ok(()),
            Some(SnapshotState::Failed) => {
                return Err(anyhow::anyhow!("Snapshot deletion failed"));
            }
            Some(_) => {}
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for snapshot to be deleted"
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn delete_snapshot(args: SnapshotDeleteArgs, client: &AnyClient) -> Result<()> {
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

    println!("Deleting snapshot \"{}\"", args.name);

    dispatch!(client, |c| {
        c.inner()
            .delete_machine_snapshot()
            .account(account)
            .machine(machine_id)
            .name(&args.name)
            .send()
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    if args.wait {
        wait_for_snapshot_deleted(machine_id, &args.name, args.wait_timeout, client).await?;
    }

    println!("Deleted snapshot \"{}\"", args.name);

    Ok(())
}

async fn boot_snapshot(args: SnapshotBootArgs, client: &AnyClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
    let id_str = machine_id.to_string();

    dispatch!(client, |c| {
        c.inner()
            .start_machine_from_snapshot()
            .account(account)
            .machine(machine_id)
            .name(&args.name)
            .send()
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    println!(
        "Booting instance {} from snapshot {}",
        &id_str[..8],
        args.name
    );

    Ok(())
}
