// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance migration commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_api::{Migration, MigrationAction, MigrationEstimate, MigrationState};

use crate::client::AnyClient;
use crate::output::{enum_to_display, json};
use crate::{dispatch, dispatch_with_types};

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
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
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

/// Fetch the current migration status as a canonical
/// `cloudapi_api::Migration` so the rendering logic stays
/// variant-agnostic.
async fn fetch_migration(
    client: &AnyClient,
    account: &str,
    instance_id: uuid::Uuid,
) -> Result<Migration> {
    let migration: Migration = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_migration()
            .account(account)
            .machine(instance_id)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Migration>(serde_json::to_value(&resp)?)?
    });
    Ok(migration)
}

async fn get_migration(args: MigrationGetArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;
    let migration = fetch_migration(client, account, instance_id).await?;

    if use_json {
        json::print_json(&migration)?;
    } else {
        println!("Instance:   {}", migration.vm_uuid);
        println!("State:      {}", enum_to_display(&migration.state));
        println!("Phase:      {}", enum_to_display(&migration.phase));
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
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let estimate: MigrationEstimate = dispatch!(client, |c| {
        let resp = c
            .inner()
            .migrate_machine_estimate()
            .account(account)
            .machine(instance_id)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<MigrationEstimate>(serde_json::to_value(&resp)?)?
    });

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

/// Dispatch a migration action (`begin` / `sync` / `switch` / `abort`)
/// and return the resulting Migration object in canonical form.
async fn post_migrate(
    client: &AnyClient,
    account: &str,
    instance_id: uuid::Uuid,
    action: MigrationAction,
    affinity: Option<&[String]>,
) -> Result<Migration> {
    let affinity_opt = affinity.filter(|a| !a.is_empty()).map(|a| a.to_vec());
    let migration: Migration = dispatch_with_types!(client, |c, t| {
        // Round-trip the canonical `MigrationAction` enum into the
        // per-client one via serde — both variants serialize to
        // identical wire strings.
        let client_action: t::MigrationAction =
            serde_json::from_value(serde_json::to_value(&action)?)?;
        let body = t::MigrateRequest {
            action: client_action,
            affinity: affinity_opt.clone(),
        };
        let resp = c
            .inner()
            .migrate()
            .account(account)
            .machine(instance_id)
            .body(body)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Migration>(serde_json::to_value(&resp)?)?
    });
    Ok(migration)
}

async fn begin_migration(
    args: MigrationBeginArgs,
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;
    let id_str = instance_id.to_string();

    let migration = post_migrate(
        client,
        account,
        instance_id,
        MigrationAction::Begin,
        args.affinity.as_deref(),
    )
    .await?;

    if args.wait {
        wait_for_action(
            instance_id,
            MigrationAction::Begin,
            args.wait_timeout,
            client,
        )
        .await?;
        eprintln!("Done - begin finished");
    } else if !args.quiet {
        if use_json {
            json::print_json(&migration)?;
        } else {
            println!("Migration started for instance {}", &id_str[..8]);
            println!("State: {}", enum_to_display(&migration.state));
            println!("Phase: {}", enum_to_display(&migration.phase));
        }
    }

    Ok(())
}

async fn sync_migration(args: MigrationSyncArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let migration = post_migrate(client, account, instance_id, MigrationAction::Sync, None).await?;

    if args.wait {
        wait_for_action(
            instance_id,
            MigrationAction::Sync,
            args.wait_timeout,
            client,
        )
        .await?;
        eprintln!("Done - sync finished");
    } else if use_json {
        json::print_json(&migration)?;
    } else {
        println!("State: {}", enum_to_display(&migration.state));
        println!("Phase: {}", enum_to_display(&migration.phase));
    }

    Ok(())
}

async fn switch_migration(
    args: MigrationSwitchArgs,
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;

    let migration =
        post_migrate(client, account, instance_id, MigrationAction::Switch, None).await?;

    if args.wait {
        wait_for_action(
            instance_id,
            MigrationAction::Switch,
            args.wait_timeout,
            client,
        )
        .await?;
        eprintln!("Done - switch finished");
    } else if use_json {
        json::print_json(&migration)?;
    } else {
        println!("State: {}", enum_to_display(&migration.state));
        println!("Phase: {}", enum_to_display(&migration.phase));
    }

    Ok(())
}

/// Wait for a migration action to complete
async fn wait_for_action(
    instance_id: uuid::Uuid,
    action: MigrationAction,
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<()> {
    let account = client.effective_account();
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let action_display = enum_to_display(&action);

    loop {
        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Migration {} timed out after {} seconds",
                action_display,
                timeout_secs
            ));
        }

        let migration = fetch_migration(client, account, instance_id).await?;

        match migration.state {
            MigrationState::Successful | MigrationState::Finished | MigrationState::Paused => {
                return Ok(());
            }
            MigrationState::Failed | MigrationState::Aborted => {
                return Err(anyhow::anyhow!(
                    "Migration {}: state={}, phase={}",
                    action_display,
                    enum_to_display(&migration.state),
                    enum_to_display(&migration.phase),
                ));
            }
            _ => {
                // Still in progress
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn wait_migration(args: MigrationWaitArgs, client: &AnyClient) -> Result<()> {
    let account = client.effective_account();
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

        let migration = fetch_migration(client, account, instance_id).await?;

        match migration.state {
            MigrationState::Successful | MigrationState::Finished => {
                println!("\nMigration completed successfully!");
                return Ok(());
            }
            MigrationState::Failed | MigrationState::Aborted => {
                return Err(anyhow::anyhow!(
                    "Migration {}: phase={}",
                    enum_to_display(&migration.state),
                    enum_to_display(&migration.phase),
                ));
            }
            _ => {
                if let Some(progress) = migration.progress_percent {
                    print!(
                        "\rProgress: {:.1}% (phase: {})   ",
                        progress,
                        enum_to_display(&migration.phase),
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
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let instance_id = super::get::resolve_instance(&args.instance, client).await?;
    let id_str = instance_id.to_string();

    let migration =
        post_migrate(client, account, instance_id, MigrationAction::Abort, None).await?;

    if args.wait {
        wait_for_action(
            instance_id,
            MigrationAction::Abort,
            args.wait_timeout,
            client,
        )
        .await?;
        eprintln!("Done - abort finished");
    } else if use_json {
        json::print_json(&migration)?;
    } else {
        println!("Migration aborted for instance {}", &id_str[..8]);
        println!("State: {}", enum_to_display(&migration.state));
        println!("Phase: {}", enum_to_display(&migration.phase));
    }

    Ok(())
}
