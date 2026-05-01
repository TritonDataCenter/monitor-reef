// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tcadm — Triton Cloud operator CLI.
//!
//! Phase 0e ships:
//!
//! * `tcadm bootstrap` — health-check ping (anonymous-allowed).
//! * `tcadm configure` / `tcadm login` / `tcadm logout` — interactive
//!   login flow that persists tokens at `~/.config/tcadm/config.json`.
//! * `tcadm env` — emit shell exports so the access token can be
//!   embedded in `eval "$(tcadm env)"` style invocations.
//! * `tcadm api-key {create,list,delete}` — long-lived bearer
//!   credentials for automation. The plaintext is shown once at
//!   creation and never persisted server-side.
//!
//! `--endpoint` and `--api-key` are global flags; they short-circuit
//! the on-disk config in priority order documented in
//! [`crate::session`].

mod commands;
mod config;
mod session;

use anyhow::Result;
use clap::{Parser, Subcommand};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "tcadm")]
#[command(about = "Triton Cloud operator CLI", long_about = None)]
#[command(version)]
struct Cli {
    /// Override the cluster endpoint. Falls back to `TCADM_ENDPOINT`
    /// then to the `endpoint` field in the on-disk config.
    #[arg(long, global = true)]
    endpoint: Option<String>,

    /// Authenticate with this API key instead of the stored login
    /// session. Falls back to `TCADM_API_KEY` if not given on the
    /// command line.
    #[arg(long, global = true)]
    api_key: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Verify that a tritond control plane is reachable.
    Bootstrap {
        /// Endpoint to probe; defaults to http://localhost:8080 if no
        /// global `--endpoint` / `TCADM_ENDPOINT` is set.
        #[arg(long)]
        endpoint: Option<String>,
        /// Emit JSON instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
    /// Interactive login: prompts for endpoint + username + password
    /// and writes `~/.config/tcadm/config.json`.
    Configure {
        /// Skip the endpoint prompt.
        #[arg(long)]
        endpoint: Option<String>,
        /// Skip the username prompt.
        #[arg(long)]
        username: Option<String>,
        /// Read the password from stdin (one line) instead of the TTY.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Re-authenticate against the stored endpoint, e.g. after the
    /// refresh token has expired.
    Login {
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password_stdin: bool,
    },
    /// Delete the on-disk config (forgets endpoint and tokens).
    Logout,
    /// Print shell `export` lines for the current session.
    Env,
    /// Manage long-lived API keys.
    ApiKey {
        #[command(subcommand)]
        command: ApiKeyCommand,
    },
    /// Inspect and verify the audit log.
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },
}

#[derive(Subcommand)]
enum AuditCommand {
    /// Page through audit events.
    List {
        /// Return events with seq > after_seq.
        #[arg(long)]
        after_seq: Option<u64>,
        /// Maximum events to return (default 100, max 1000).
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Fetch a single audit event by sequence.
    Get {
        seq: u64,
        #[arg(long)]
        json: bool,
    },
    /// Walk the chain and check hash integrity.
    Verify {
        #[arg(long)]
        from: Option<u64>,
        #[arg(long)]
        to: Option<u64>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ApiKeyCommand {
    /// Mint a new API key. The plaintext is shown once.
    Create {
        /// Free-text label, e.g. `ci-pipeline`.
        #[arg(long)]
        description: String,
        /// Emit JSON instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
    /// List the calling user's API keys (no secret material).
    List {
        #[arg(long)]
        json: bool,
    },
    /// Delete one of the calling user's API keys by id.
    Delete { api_key_id: Uuid },
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
        Commands::Bootstrap {
            endpoint: subcmd_endpoint,
            json,
        } => {
            let endpoint = subcmd_endpoint
                .or(cli.endpoint)
                .or_else(|| std::env::var("TCADM_ENDPOINT").ok())
                .unwrap_or_else(|| "http://localhost:8080".to_string());
            commands::bootstrap(&endpoint, json).await
        }
        Commands::Configure {
            endpoint,
            username,
            password_stdin,
        } => {
            let endpoint = endpoint
                .or(cli.endpoint)
                .or_else(|| std::env::var("TCADM_ENDPOINT").ok());
            commands::configure(endpoint, username, password_stdin).await
        }
        Commands::Login {
            endpoint,
            username,
            password_stdin,
        } => {
            let endpoint = endpoint
                .or(cli.endpoint)
                .or_else(|| std::env::var("TCADM_ENDPOINT").ok());
            commands::login(endpoint, username, password_stdin).await
        }
        Commands::Logout => commands::logout(),
        Commands::Env => commands::env(cli.endpoint, cli.api_key).await,
        Commands::ApiKey { command } => match command {
            ApiKeyCommand::Create { description, json } => {
                commands::api_key_create(cli.endpoint, cli.api_key, description, json).await
            }
            ApiKeyCommand::List { json } => {
                commands::api_key_list(cli.endpoint, cli.api_key, json).await
            }
            ApiKeyCommand::Delete { api_key_id } => {
                commands::api_key_delete(cli.endpoint, cli.api_key, api_key_id).await
            }
        },
        Commands::Audit { command } => match command {
            AuditCommand::List {
                after_seq,
                limit,
                json,
            } => commands::audit_list(cli.endpoint, cli.api_key, after_seq, limit, json).await,
            AuditCommand::Get { seq, json } => {
                commands::audit_get(cli.endpoint, cli.api_key, seq, json).await
            }
            AuditCommand::Verify { from, to, json } => {
                commands::audit_verify(cli.endpoint, cli.api_key, from, to, json).await
            }
        },
    }
}
