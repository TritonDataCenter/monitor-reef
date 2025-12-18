// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance migration commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json;

#[derive(Subcommand, Clone)]
pub enum MigrationCommand {
    /// Get the current migration status of an instance
    Get(MigrationGetArgs),
    /// List migrations (alias for get)
    #[command(visible_alias = "ls")]
    List(MigrationGetArgs),
    /// Estimate migration for an instance
    Estimate(MigrationEstimateArgs),
    /// Start/begin migration of an instance
    #[command(visible_alias = "start")]
    Begin(MigrationBeginArgs),
    /// Sync migration data
    Sync(MigrationSyncArgs),
    /// Switch to migrated instance (finalize)
    #[command(visible_alias = "finalize")]
    Switch(MigrationSwitchArgs),
    /// Wait for a migration to complete
    Wait(MigrationWaitArgs),
    /// Abort an in-progress migration
    Abort(MigrationAbortArgs),
}

#[derive(Args, Clone)]
pub struct MigrationGetArgs {
    /// Instance ID or name
    pub instance: String,
}

#[derive(Args, Clone)]
pub struct MigrationEstimateArgs {
    /// Instance ID or name
    pub instance: String,
}

#[derive(Args, Clone)]
pub struct MigrationBeginArgs {
    /// Instance ID or name
    pub instance: String,

    /// Affinity rules (can be specified multiple times)
    #[arg(short = 'a', long)]
    pub affinity: Option<Vec<String>>,

    /// Wait for action to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "1800")]
    pub wait_timeout: u64,

    /// Suppress output after starting migration
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,
}

#[derive(Args, Clone)]
pub struct MigrationSyncArgs {
    /// Instance ID or name
    pub instance: String,

    /// Wait for action to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "1800")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct MigrationSwitchArgs {
    /// Instance ID or name
    pub instance: String,

    /// Wait for action to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "1800")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct MigrationWaitArgs {
    /// Instance ID or name
    pub instance: String,

    /// Wait timeout in seconds
    #[arg(long, default_value = "1800")]
    pub timeout: u64,
}

#[derive(Args, Clone)]
pub struct MigrationAbortArgs {
    /// Instance ID or name
    pub instance: String,

    /// Wait for action to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "1800")]
    pub wait_timeout: u64,
}

impl MigrationCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Get(args) | Self::List(args) => get_migration(args, client, use_json).await,
            Self::Estimate(args) => estimate_migration(args, client, use_json).await,
            Self::Begin(args) => begin_migration(args, client, use_json).await,
            Self::Sync(args) => sync_migration(args, client, use_json).await,
            Self::Switch(args) => switch_migration(args, client, use_json).await,
            Self::Wait(args) => wait_migration(args, client).await,
            Self::Abort(args) => abort_migration(args, client, use_json).await,
        }
    }
}

async fn get_migration(args: MigrationGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let response = client
        .inner()
        .get_migration()
        .account(account)
        .machine(&instance_id)
        .send()
        .await?;

    let migration = response.into_inner();

    if use_json {
        json::print_json(&migration)?;
    } else {
        println!("Instance:   {}", migration.vm_uuid);
        println!("State:      {}", migration.state);
        println!("Phase:      {}", migration.phase);
        if let Some(progress) = migration.progress_percent {
            println!("Progress:   {:.1}%", progress);
        }
        println!("Created:    {}", migration.created_timestamp);
        if let Some(updated) = &migration.updated_timestamp {
            println!("Updated:    {}", updated);
        }
        if let Some(auto) = migration.automatic {
            println!("Automatic:  {}", auto);
        }
    }

    Ok(())
}

async fn estimate_migration(
    args: MigrationEstimateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let response = client
        .inner()
        .migrate_machine_estimate()
        .account(account)
        .machine(&instance_id)
        .send()
        .await?;

    let estimate = response.into_inner();

    if use_json {
        json::print_json(&estimate)?;
    } else {
        let size_gb = estimate.size as f64 / 1_073_741_824.0;
        println!("Estimated migration size: {:.2} GB", size_gb);
        if let Some(duration) = estimate.duration {
            let minutes = duration / 60;
            let seconds = duration % 60;
            println!("Estimated duration:       {}m {}s", minutes, seconds);
        }
    }

    Ok(())
}

async fn begin_migration(
    args: MigrationBeginArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let request = cloudapi_client::types::MigrateRequest {
        action: cloudapi_client::types::MigrationAction::Begin,
        affinity: args.affinity,
    };

    let response = client
        .inner()
        .migrate()
        .account(account)
        .machine(&instance_id)
        .body(request)
        .send()
        .await?;

    let migration = response.into_inner();

    if args.wait {
        // Wait for the action to complete
        wait_for_action(&instance_id, "begin", args.wait_timeout, client).await?;
        // Output node-triton compatible message
        println!("Done - begin finished");
    } else if !args.quiet {
        if use_json {
            json::print_json(&migration)?;
        } else {
            println!("Migration started for instance {}", &instance_id[..8]);
            println!("State: {}", migration.state);
            println!("Phase: {}", migration.phase);
        }
    }

    Ok(())
}

async fn sync_migration(
    args: MigrationSyncArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let request = cloudapi_client::types::MigrateRequest {
        action: cloudapi_client::types::MigrationAction::Sync,
        affinity: None,
    };

    let response = client
        .inner()
        .migrate()
        .account(account)
        .machine(&instance_id)
        .body(request)
        .send()
        .await?;

    let migration = response.into_inner();

    if args.wait {
        wait_for_action(&instance_id, "sync", args.wait_timeout, client).await?;
        println!("Done - sync finished");
    } else if use_json {
        json::print_json(&migration)?;
    } else {
        println!("State: {}", migration.state);
        println!("Phase: {}", migration.phase);
    }

    Ok(())
}

async fn switch_migration(
    args: MigrationSwitchArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let request = cloudapi_client::types::MigrateRequest {
        action: cloudapi_client::types::MigrationAction::Switch,
        affinity: None,
    };

    let response = client
        .inner()
        .migrate()
        .account(account)
        .machine(&instance_id)
        .body(request)
        .send()
        .await?;

    let migration = response.into_inner();

    if args.wait {
        wait_for_action(&instance_id, "switch", args.wait_timeout, client).await?;
        println!("Done - switch finished");
    } else if use_json {
        json::print_json(&migration)?;
    } else {
        println!("State: {}", migration.state);
        println!("Phase: {}", migration.phase);
    }

    Ok(())
}

/// Wait for a migration action to complete
async fn wait_for_action(
    instance_id: &str,
    action: &str,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    let account = &client.auth_config().account;
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);

    loop {
        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Migration {} timed out after {} seconds",
                action,
                timeout_secs
            ));
        }

        let response = client
            .inner()
            .get_migration()
            .account(account)
            .machine(instance_id)
            .send()
            .await?;

        let migration = response.into_inner();

        match migration.state.as_str() {
            "successful" | "finished" | "paused" => {
                // Action completed
                return Ok(());
            }
            "failed" | "aborted" => {
                return Err(anyhow::anyhow!(
                    "Migration {}: state={}, phase={}",
                    action,
                    migration.state,
                    migration.phase
                ));
            }
            _ => {
                // Still in progress
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn wait_migration(args: MigrationWaitArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    println!("Waiting for migration to complete...");

    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(args.timeout);

    loop {
        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Migration wait timed out after {} seconds",
                args.timeout
            ));
        }

        let response = client
            .inner()
            .get_migration()
            .account(account)
            .machine(&instance_id)
            .send()
            .await?;

        let migration = response.into_inner();

        match migration.state.as_str() {
            "successful" | "finished" => {
                println!("\nMigration completed successfully!");
                return Ok(());
            }
            "failed" | "aborted" => {
                return Err(anyhow::anyhow!(
                    "Migration {}: phase={}",
                    migration.state,
                    migration.phase
                ));
            }
            _ => {
                if let Some(progress) = migration.progress_percent {
                    print!(
                        "\rProgress: {:.1}% (phase: {})   ",
                        progress, migration.phase
                    );
                    use std::io::Write;
                    std::io::stdout().flush()?;
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn abort_migration(
    args: MigrationAbortArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let request = cloudapi_client::types::MigrateRequest {
        action: cloudapi_client::types::MigrationAction::Abort,
        affinity: None,
    };

    let response = client
        .inner()
        .migrate()
        .account(account)
        .machine(&instance_id)
        .body(request)
        .send()
        .await?;

    let migration = response.into_inner();

    if args.wait {
        wait_for_action(&instance_id, "abort", args.wait_timeout, client).await?;
        println!("Done - abort finished");
    } else if use_json {
        json::print_json(&migration)?;
    } else {
        println!("Migration aborted for instance {}", &instance_id[..8]);
        println!("State: {}", migration.state);
        println!("Phase: {}", migration.phase);
    }

    Ok(())
}
