// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance snapshot subcommands

use anyhow::Result;
use clap::{Args, Subcommand};
use dialoguer::Confirm;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::{Snapshot, SnapshotState};

use crate::define_columns;
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

    let mut snapshots = response.into_inner();
    snapshots.sort_by(|a, b| a.name.cmp(&b.name));

    if use_json {
        json::print_json_stream(&snapshots)?;
    } else {
        define_columns! {
            SnapshotColumn for Snapshot, long_from: 3, {
                Name("NAME") => |snap| snap.name.clone(),
                State("STATE") => |snap| crate::output::enum_to_display(&snap.state),
                Created("CREATED") => |snap| snap.created.map(|c| c.to_rfc3339()).unwrap_or_else(|| "-".to_string()),
                // --- long-only columns below ---
                Updated("UPDATED") => |snap| {
                    snap.updated.map(|u| u.to_rfc3339()).unwrap_or_else(|| "-".to_string())
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

    let request = triton_gateway_client::types::CreateSnapshotRequest {
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
    client: &TypedClient,
) -> Result<triton_gateway_client::types::Snapshot> {
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

async fn wait_for_snapshot_deleted(
    machine_id: uuid::Uuid,
    snapshot_name: &str,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    // Poll the list endpoint rather than the individual get endpoint.
    // CloudAPI may continue returning the snapshot via GET (with state
    // "created") long after it has been removed from the list response.
    loop {
        let response = client
            .inner()
            .list_machine_snapshots()
            .account(account)
            .machine(machine_id)
            .send()
            .await?;

        let snapshots = response.into_inner();
        let found = snapshots.iter().find(|s| s.name == snapshot_name);

        match found {
            None => return Ok(()),
            Some(snap) => {
                if snap.state == SnapshotState::Deleted {
                    return Ok(());
                }
                if snap.state == SnapshotState::Failed {
                    return Err(anyhow::anyhow!("Snapshot deletion failed"));
                }
            }
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for snapshot to be deleted"
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

    println!("Deleting snapshot \"{}\"", args.name);

    client
        .inner()
        .delete_machine_snapshot()
        .account(account)
        .machine(machine_id)
        .name(&args.name)
        .send()
        .await?;

    if args.wait {
        wait_for_snapshot_deleted(machine_id, &args.name, args.wait_timeout, client).await?;
    }

    println!("Deleted snapshot \"{}\"", args.name);

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: start a mock HTTP server that returns `body` for every request.
    async fn mock_server(body: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle = tokio::spawn(async move {
            for _ in 0..10 {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });

        (port, handle)
    }

    /// Helper: construct a TypedClient pointing at a local mock server.
    fn test_client(port: u16) -> TypedClient {
        use std::path::PathBuf;

        let key_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../libs/triton-auth/tests/keys/id_rsa.pem");
        let auth = triton_auth::AuthConfig {
            account: "testaccount".to_string(),
            user: None,
            key_source: triton_auth::KeySource::File {
                path: key_path,
                passphrase: None,
            },
            roles: None,
            act_as: None,
            accept_version: None,
        };
        TypedClient::new(
            &format!("http://127.0.0.1:{port}"),
            triton_gateway_client::GatewayAuthConfig::ssh_key(auth),
        )
    }

    /// Snapshot absent from list → deletion complete.
    #[tokio::test]
    async fn test_wait_for_snapshot_deleted_empty_list() {
        let (port, server) = mock_server("[]").await;
        let client = test_client(port);

        let result = wait_for_snapshot_deleted(uuid::Uuid::nil(), "test-snap", 5, &client).await;

        server.abort();
        assert!(
            result.is_ok(),
            "empty snapshot list should mean deletion complete, got: {:?}",
            result.err()
        );
    }

    /// Snapshot present in list with state "deleted" → deletion complete.
    #[tokio::test]
    async fn test_wait_for_snapshot_deleted_recognizes_deleted_state() {
        let (port, server) = mock_server(r#"[{"name":"test-snap","state":"deleted"}]"#).await;
        let client = test_client(port);

        let result = wait_for_snapshot_deleted(uuid::Uuid::nil(), "test-snap", 5, &client).await;

        server.abort();
        assert!(
            result.is_ok(),
            "SnapshotState::Deleted should mean deletion complete, got: {:?}",
            result.err()
        );
    }

    /// Other snapshots present but target absent → deletion complete.
    #[tokio::test]
    async fn test_wait_for_snapshot_deleted_other_snapshots_remain() {
        let (port, server) = mock_server(r#"[{"name":"other-snap","state":"created"}]"#).await;
        let client = test_client(port);

        let result = wait_for_snapshot_deleted(uuid::Uuid::nil(), "test-snap", 5, &client).await;

        server.abort();
        assert!(
            result.is_ok(),
            "target snapshot absent from list should mean deletion complete, got: {:?}",
            result.err()
        );
    }
}
