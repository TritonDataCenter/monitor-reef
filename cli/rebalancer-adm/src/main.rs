// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Rebalancer administration CLI
//!
//! A command-line tool for managing rebalancer jobs. Supports creating,
//! listing, querying, and retrying evacuation jobs.

use anyhow::Result;
use clap::{Parser, Subcommand};
use rebalancer_manager_client::{types, Client};

#[derive(Parser)]
#[command(name = "rebalancer-adm")]
#[command(about = "Rebalancer client utility", long_about = None)]
#[command(version)]
struct Cli {
    /// Base URL of the rebalancer manager service
    #[arg(long, default_value = "http://localhost:80", env = "REBALANCER_URL")]
    base_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Job operations
    Job {
        #[command(subcommand)]
        action: JobAction,
    },
}

#[derive(Subcommand)]
enum JobAction {
    /// List all known rebalancer jobs
    List,

    /// Get information on a specific job
    Get {
        /// UUID of the job
        uuid: String,
    },

    /// Retry a previously run and completed job
    Retry {
        /// UUID of the previous job
        uuid: String,
    },

    /// Create a rebalancer job
    Create {
        #[command(subcommand)]
        job_type: CreateJobType,
    },
}

#[derive(Subcommand)]
enum CreateJobType {
    /// Create an evacuate job
    Evacuate {
        /// Storage node to evacuate (e.g., "1.stor.domain.com")
        #[arg(short, long)]
        shark: String,

        /// Maximum number of objects to process (for testing)
        #[arg(short, long)]
        max_objects: Option<u32>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(&cli.base_url);

    match cli.command {
        Commands::Job { action } => match action {
            JobAction::List => {
                let response = client.list_jobs().send().await.map_err(|e| {
                    anyhow::anyhow!("Failed to list jobs: {}", e)
                })?;

                let jobs = response.into_inner();

                if jobs.is_empty() {
                    println!("No jobs found.");
                } else {
                    println!("{:<40} {:<12} {:<10}", "ID", "ACTION", "STATE");
                    println!("{}", "-".repeat(64));
                    for job in jobs {
                        println!(
                            "{:<40} {:<12} {:<10}",
                            job.id,
                            format_action(&job.action),
                            format_state(&job.state),
                        );
                    }
                }
            }

            JobAction::Get { uuid } => {
                let response = client
                    .get_job()
                    .uuid(&uuid)
                    .send()
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to get job '{}': {}", uuid, e)
                    })?;

                let status = response.into_inner();

                println!("Job Status");
                println!("{}", "=".repeat(50));
                println!();
                println!("State: {}", format_state(&status.state));
                println!();

                // Display config
                println!("Configuration:");
                println!("{}", "-".repeat(50));
                match &status.config {
                    types::JobStatusConfig::Evacuate(from_shark) => {
                        println!("  Action:     Evacuate");
                        println!(
                            "  From Shark: {} ({})",
                            from_shark.manta_storage_id, from_shark.datacenter
                        );
                    }
                }
                println!();

                // Display results
                println!("Results:");
                println!("{}", "-".repeat(50));
                // JobStatusResults is a newtype around HashMap<String, i64>
                if status.results.0.is_empty() {
                    println!("  No results yet.");
                } else {
                    for (status_name, count) in &status.results.0 {
                        println!("  {:<20} {}", status_name, count);
                    }
                }
            }

            JobAction::Retry { uuid } => {
                let response = client
                    .retry_job()
                    .uuid(&uuid)
                    .send()
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to retry job '{}': {}", uuid, e)
                    })?;

                let new_uuid = response.into_inner();
                println!("Retry job created: {}", new_uuid);
            }

            JobAction::Create { job_type } => match job_type {
                CreateJobType::Evacuate { shark, max_objects } => {
                    let payload = types::JobPayload::Evacuate(
                        types::EvacuateJobPayload {
                            from_shark: shark.clone(),
                            max_objects,
                        },
                    );

                    let response = client
                        .create_job()
                        .body(payload)
                        .send()
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "Failed to create evacuate job for '{}': {}",
                                shark,
                                e
                            )
                        })?;

                    let job_uuid = response.into_inner();
                    println!("Evacuate job created: {}", job_uuid);
                }
            },
        },
    }

    Ok(())
}

fn format_action(action: &types::JobAction) -> &'static str {
    match action {
        types::JobAction::Evacuate => "Evacuate",
        types::JobAction::None => "None",
    }
}

fn format_state(state: &types::JobState) -> &'static str {
    match state {
        types::JobState::Init => "Init",
        types::JobState::Setup => "Setup",
        types::JobState::Running => "Running",
        types::JobState::Stopped => "Stopped",
        types::JobState::Complete => "Complete",
        types::JobState::Failed => "Failed",
    }
}
