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
    /// Estimate migration for an instance
    Estimate(MigrationEstimateArgs),
    /// Start migration of an instance
    Start(MigrationStartArgs),
    /// Wait for a migration to complete
    Wait(MigrationWaitArgs),
    /// Finalize (switch to) a migrated instance
    Finalize(MigrationFinalizeArgs),
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
pub struct MigrationStartArgs {
    /// Instance ID or name
    pub instance: String,

    /// Affinity rules (can be specified multiple times)
    #[arg(short = 'a', long)]
    pub affinity: Option<Vec<String>>,
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
pub struct MigrationFinalizeArgs {
    /// Instance ID or name
    pub instance: String,
}

#[derive(Args, Clone)]
pub struct MigrationAbortArgs {
    /// Instance ID or name
    pub instance: String,
}

impl MigrationCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Get(args) => get_migration(args, client, use_json).await,
            Self::Estimate(args) => estimate_migration(args, client, use_json).await,
            Self::Start(args) => start_migration(args, client, use_json).await,
            Self::Wait(args) => wait_migration(args, client).await,
            Self::Finalize(args) => finalize_migration(args, client, use_json).await,
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

async fn start_migration(
    args: MigrationStartArgs,
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

    println!("Migration started for instance {}", &instance_id[..8]);

    if use_json {
        json::print_json(&migration)?;
    } else {
        println!("State: {}", migration.state);
        println!("Phase: {}", migration.phase);
    }

    Ok(())
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

async fn finalize_migration(
    args: MigrationFinalizeArgs,
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

    println!("Migration finalized for instance {}", &instance_id[..8]);

    if use_json {
        json::print_json(&migration)?;
    } else {
        println!("State: {}", migration.state);
        println!("Phase: {}", migration.phase);
    }

    Ok(())
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

    println!("Migration aborted for instance {}", &instance_id[..8]);

    if use_json {
        json::print_json(&migration)?;
    } else {
        println!("State: {}", migration.state);
        println!("Phase: {}", migration.phase);
    }

    Ok(())
}
