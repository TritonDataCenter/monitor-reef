// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tcadm — Triton Cloud operator CLI.
//!
//! Phase 0 ships a single subcommand, `bootstrap`, that verifies a
//! freshly deployed `tritond` is reachable and serving the expected
//! version. Subsequent phases extend this into a full lifecycle tool
//! (FoundationDB schema init, root credential issuance, silo
//! provisioning, Cedar policy upload).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tcadm")]
#[command(about = "Triton Cloud operator CLI", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Verify that a tritond control plane is reachable.
    ///
    /// Phase 0 contract: hit `/v2/health`, print the reported status
    /// and version, exit 0 on success.
    Bootstrap {
        /// Base URL of the tritond control plane.
        #[arg(
            long,
            env = "TRITOND_ENDPOINT",
            default_value = "http://localhost:8080"
        )]
        endpoint: String,

        /// Emit the raw JSON response instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Bootstrap { endpoint, json } => bootstrap(&endpoint, json).await,
    }
}

async fn bootstrap(endpoint: &str, json_output: bool) -> Result<()> {
    let client = tritond_client::Client::new(endpoint);

    let response = client
        .health()
        .send()
        .await
        .with_context(|| format!("failed to reach tritond at {endpoint}"))?;
    let body = response.into_inner();

    if json_output {
        let payload = serde_json::json!({
            "endpoint": endpoint,
            "status": body.status,
            "version": body.version,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("tritond at {endpoint}");
        println!("  status:  {}", body.status);
        println!("  version: {}", body.version);
    }

    if body.status != "ok" {
        anyhow::bail!("tritond reported non-ok status: {}", body.status);
    }

    Ok(())
}
