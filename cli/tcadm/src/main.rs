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
    /// Manage per-silo identity-provider configuration.
    Silo {
        #[command(subcommand)]
        command: SiloCommand,
    },
}

#[derive(Subcommand)]
enum SiloCommand {
    /// Manage the silo's OIDC identity provider.
    Idp {
        #[command(subcommand)]
        command: SiloIdpCommand,
    },
    /// Manage projects inside a silo.
    Project {
        #[command(subcommand)]
        command: SiloProjectCommand,
    },
    /// Manage SSH keys registered in the silo's catalog.
    SshKey {
        #[command(subcommand)]
        command: SiloSshKeyCommand,
    },
    /// Manage images registered in the silo's catalog.
    Image {
        #[command(subcommand)]
        command: SiloImageCommand,
    },
}

#[derive(Subcommand)]
enum SiloImageCommand {
    /// List images in the silo's catalog.
    List {
        silo_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Register a new image. The server validates `sha256` shape
    /// and rejects zero-byte content.
    Add {
        silo_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        os: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        size_bytes: u64,
        /// SHA-256 of the image content; must be 64 lowercase hex chars.
        #[arg(long)]
        sha256: String,
        /// Optional URL where the image content lives.
        #[arg(long)]
        source_url: Option<String>,
        /// Optionally pin the image UUID instead of letting the
        /// server generate one. Used when tritond's image id needs
        /// to equal the corresponding `imgadm` UUID on every CN
        /// (so the per-CN agent passes it straight to vmadm).
        #[arg(long)]
        id: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single image.
    Get {
        silo_id: Uuid,
        image_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete an image.
    Delete { silo_id: Uuid, image_id: Uuid },
}

#[derive(Subcommand)]
enum SiloSshKeyCommand {
    /// List SSH keys in the silo's catalog.
    List {
        silo_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Register a new SSH key. Reads the openssh public-key string
    /// from `--public-key-file` (one line) or `--public-key`.
    Add {
        silo_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// OpenSSH public key string (e.g. `ssh-ed25519 AAAA... user@host`).
        #[arg(long, conflicts_with = "public_key_file")]
        public_key: Option<String>,
        /// Path to a file containing the openssh public key (one line).
        #[arg(long, conflicts_with = "public_key")]
        public_key_file: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single SSH key.
    Get {
        silo_id: Uuid,
        ssh_key_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete an SSH key.
    Delete { silo_id: Uuid, ssh_key_id: Uuid },
}

#[derive(Subcommand)]
enum SiloProjectCommand {
    /// List projects in the silo.
    List {
        silo_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a new project.
    Create {
        silo_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        json: bool,
    },
    /// Read a single project.
    Get {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a project.
    Delete { silo_id: Uuid, project_id: Uuid },
    /// Manage VPCs inside a project.
    Vpc {
        #[command(subcommand)]
        command: SiloProjectVpcCommand,
    },
    /// Manage instances inside a project.
    Instance {
        #[command(subcommand)]
        command: SiloProjectInstanceCommand,
    },
    /// Manage the project's resource quota.
    Quota {
        #[command(subcommand)]
        command: SiloProjectQuotaCommand,
    },
    /// Manage floating IPs (project-scoped, allocated from a fleet
    /// pool, attachable to any NIC in the project).
    FloatingIp {
        #[command(subcommand)]
        command: SiloProjectFloatingIpCommand,
    },
}

#[derive(Subcommand)]
enum SiloProjectFloatingIpCommand {
    /// List floating IPs in the project.
    List {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Allocate a new floating IP from the fleet pool.
    Create {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// Address family. Valid values: `v4` or `v6`.
        #[arg(long, default_value = "v4")]
        family: String,
        #[arg(long)]
        json: bool,
    },
    /// Read a single floating IP.
    Get {
        silo_id: Uuid,
        project_id: Uuid,
        floating_ip_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Release a floating IP back to its pool. Returns 409 if
    /// currently attached.
    Delete {
        silo_id: Uuid,
        project_id: Uuid,
        floating_ip_id: Uuid,
    },
    /// Attach a floating IP to a NIC. Replace semantics — if the
    /// IP was already attached elsewhere, it swaps atomically.
    Attach {
        silo_id: Uuid,
        project_id: Uuid,
        floating_ip_id: Uuid,
        #[arg(long)]
        nic_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Detach a floating IP. Idempotent.
    Detach {
        silo_id: Uuid,
        project_id: Uuid,
        floating_ip_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SiloProjectInstanceCommand {
    /// List instances in the project.
    List {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a new instance.
    Create {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        image_id: Uuid,
        #[arg(long)]
        primary_subnet_id: Uuid,
        /// Repeatable: SSH keys to inject at first boot.
        #[arg(long = "ssh-key-id")]
        ssh_key_ids: Vec<Uuid>,
        #[arg(long)]
        cpu: u32,
        #[arg(long)]
        memory_bytes: u64,
        #[arg(long)]
        json: bool,
    },
    /// Read a single instance.
    Get {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete an instance (must be Stopped or Failed).
    Delete {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
    },
    /// Start a Stopped instance.
    Start {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Stop a Running instance.
    Stop {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Restart a Running instance.
    Restart {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Inspect NICs attached to an instance.
    Nic {
        #[command(subcommand)]
        command: SiloProjectInstanceNicCommand,
    },
    /// Inspect disks attached to an instance.
    Disk {
        #[command(subcommand)]
        command: SiloProjectInstanceDiskCommand,
    },
}

#[derive(Subcommand)]
enum SiloProjectInstanceDiskCommand {
    /// List the disks attached to an instance.
    List {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read a single disk.
    Get {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        disk_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SiloProjectInstanceNicCommand {
    /// List the NICs attached to an instance (Phase 0 ships exactly
    /// one — the auto-created `primary` NIC).
    List {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read a single NIC.
    Get {
        silo_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        nic_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SiloProjectQuotaCommand {
    /// Set (or replace) the project's quota.
    Set {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        cpu_limit: u32,
        #[arg(long)]
        memory_bytes: u64,
        #[arg(long)]
        disk_bytes: u64,
        #[arg(long)]
        instance_limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Read the project's quota.
    Get {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Remove the project's quota (project becomes unlimited).
    Delete { silo_id: Uuid, project_id: Uuid },
}

#[derive(Subcommand)]
enum SiloProjectVpcCommand {
    /// List VPCs in a project.
    List {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a new VPC in a project. At least one of `--ipv4-block`
    /// and `--ipv6-block` must be provided.
    Create {
        silo_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// IPv4 CIDR for the VPC overlay, e.g. `10.0.0.0/24`.
        #[arg(long)]
        ipv4_block: Option<String>,
        /// IPv6 CIDR for the VPC overlay, e.g. `fd00::/48`.
        #[arg(long)]
        ipv6_block: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single VPC.
    Get {
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a VPC.
    Delete {
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
    },
    /// Manage subnets inside a VPC.
    Subnet {
        #[command(subcommand)]
        command: SiloProjectVpcSubnetCommand,
    },
}

#[derive(Subcommand)]
enum SiloProjectVpcSubnetCommand {
    /// List subnets in a VPC.
    List {
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a new subnet in a VPC. At least one of `--ipv4-block`
    /// and `--ipv6-block` must be provided. Each block must be
    /// contained in the parent VPC's same-family CIDR and must not
    /// overlap an existing subnet.
    Create {
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// IPv4 CIDR carved out of the parent VPC's ipv4_block.
        #[arg(long)]
        ipv4_block: Option<String>,
        /// IPv6 CIDR carved out of the parent VPC's ipv6_block.
        #[arg(long)]
        ipv6_block: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single subnet.
    Get {
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        subnet_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a subnet.
    Delete {
        silo_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        subnet_id: Uuid,
    },
}

#[derive(Subcommand)]
enum SiloIdpCommand {
    /// Configure (or replace) the silo's IdP. Eagerly fetches the
    /// OIDC discovery document; rejects on failure.
    Set {
        silo_id: Uuid,
        #[arg(long)]
        issuer_url: String,
        #[arg(long)]
        client_id: String,
        /// Read the client secret from stdin (one line).
        #[arg(long)]
        client_secret_stdin: bool,
        /// Pin the expected `aud` claim (defaults to client_id).
        #[arg(long)]
        audience: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read the silo's IdP config (client secret never returned).
    Get {
        silo_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Remove the silo's IdP config. Federated users in that silo
    /// will fail to authenticate until a new config is posted.
    Delete { silo_id: Uuid },
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
        /// Permission scope. `full` (default) acts as the owning
        /// user; `read-only` restricts to list/get + audit reads;
        /// `audit-only` restricts to audit-chain reads only.
        #[arg(long, value_enum, default_value_t = ApiKeyScopeArg::Full)]
        scope: ApiKeyScopeArg,
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

/// CLI mirror of [`tritond_client::types::ApiKeyScope`]. Kept
/// separate so the clap-derived value-name (`read-only`,
/// `audit-only`) can use kebab-case while the wire format stays
/// snake_case.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ApiKeyScopeArg {
    Full,
    ReadOnly,
    AuditOnly,
    Agent,
}

impl From<ApiKeyScopeArg> for tritond_client::types::ApiKeyScope {
    fn from(arg: ApiKeyScopeArg) -> Self {
        match arg {
            ApiKeyScopeArg::Full => tritond_client::types::ApiKeyScope::Full,
            ApiKeyScopeArg::ReadOnly => tritond_client::types::ApiKeyScope::ReadOnly,
            ApiKeyScopeArg::AuditOnly => tritond_client::types::ApiKeyScope::AuditOnly,
            ApiKeyScopeArg::Agent => tritond_client::types::ApiKeyScope::Agent,
        }
    }
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
            ApiKeyCommand::Create {
                description,
                scope,
                json,
            } => {
                commands::api_key_create(cli.endpoint, cli.api_key, description, scope.into(), json)
                    .await
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
        Commands::Silo { command } => match command {
            SiloCommand::Idp { command } => match command {
                SiloIdpCommand::Set {
                    silo_id,
                    issuer_url,
                    client_id,
                    client_secret_stdin,
                    audience,
                    json,
                } => {
                    commands::silo_idp_set(
                        cli.endpoint,
                        cli.api_key,
                        silo_id,
                        issuer_url,
                        client_id,
                        client_secret_stdin,
                        audience,
                        json,
                    )
                    .await
                }
                SiloIdpCommand::Get { silo_id, json } => {
                    commands::silo_idp_get(cli.endpoint, cli.api_key, silo_id, json).await
                }
                SiloIdpCommand::Delete { silo_id } => {
                    commands::silo_idp_delete(cli.endpoint, cli.api_key, silo_id).await
                }
            },
            SiloCommand::Project { command } => match command {
                SiloProjectCommand::List { silo_id, json } => {
                    commands::silo_project_list(cli.endpoint, cli.api_key, silo_id, json).await
                }
                SiloProjectCommand::Create {
                    silo_id,
                    name,
                    description,
                    json,
                } => {
                    commands::silo_project_create(
                        cli.endpoint,
                        cli.api_key,
                        silo_id,
                        name,
                        description,
                        json,
                    )
                    .await
                }
                SiloProjectCommand::Get {
                    silo_id,
                    project_id,
                    json,
                } => {
                    commands::silo_project_get(cli.endpoint, cli.api_key, silo_id, project_id, json)
                        .await
                }
                SiloProjectCommand::Delete {
                    silo_id,
                    project_id,
                } => {
                    commands::silo_project_delete(cli.endpoint, cli.api_key, silo_id, project_id)
                        .await
                }
                SiloProjectCommand::Instance { command } => match command {
                    SiloProjectInstanceCommand::List {
                        silo_id,
                        project_id,
                        json,
                    } => {
                        commands::silo_project_instance_list(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectInstanceCommand::Create {
                        silo_id,
                        project_id,
                        name,
                        description,
                        image_id,
                        primary_subnet_id,
                        ssh_key_ids,
                        cpu,
                        memory_bytes,
                        json,
                    } => {
                        commands::silo_project_instance_create(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            name,
                            description,
                            image_id,
                            primary_subnet_id,
                            ssh_key_ids,
                            cpu,
                            memory_bytes,
                            json,
                        )
                        .await
                    }
                    SiloProjectInstanceCommand::Get {
                        silo_id,
                        project_id,
                        instance_id,
                        json,
                    } => {
                        commands::silo_project_instance_get(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            instance_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectInstanceCommand::Delete {
                        silo_id,
                        project_id,
                        instance_id,
                    } => {
                        commands::silo_project_instance_delete(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            instance_id,
                        )
                        .await
                    }
                    SiloProjectInstanceCommand::Start {
                        silo_id,
                        project_id,
                        instance_id,
                        json,
                    } => {
                        commands::silo_project_instance_lifecycle(
                            cli.endpoint,
                            cli.api_key,
                            "start",
                            silo_id,
                            project_id,
                            instance_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectInstanceCommand::Stop {
                        silo_id,
                        project_id,
                        instance_id,
                        json,
                    } => {
                        commands::silo_project_instance_lifecycle(
                            cli.endpoint,
                            cli.api_key,
                            "stop",
                            silo_id,
                            project_id,
                            instance_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectInstanceCommand::Restart {
                        silo_id,
                        project_id,
                        instance_id,
                        json,
                    } => {
                        commands::silo_project_instance_lifecycle(
                            cli.endpoint,
                            cli.api_key,
                            "restart",
                            silo_id,
                            project_id,
                            instance_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectInstanceCommand::Disk { command } => match command {
                        SiloProjectInstanceDiskCommand::List {
                            silo_id,
                            project_id,
                            instance_id,
                            json,
                        } => {
                            commands::silo_project_instance_disk_list(
                                cli.endpoint,
                                cli.api_key,
                                silo_id,
                                project_id,
                                instance_id,
                                json,
                            )
                            .await
                        }
                        SiloProjectInstanceDiskCommand::Get {
                            silo_id,
                            project_id,
                            instance_id,
                            disk_id,
                            json,
                        } => {
                            commands::silo_project_instance_disk_get(
                                cli.endpoint,
                                cli.api_key,
                                silo_id,
                                project_id,
                                instance_id,
                                disk_id,
                                json,
                            )
                            .await
                        }
                    },
                    SiloProjectInstanceCommand::Nic { command } => match command {
                        SiloProjectInstanceNicCommand::List {
                            silo_id,
                            project_id,
                            instance_id,
                            json,
                        } => {
                            commands::silo_project_instance_nic_list(
                                cli.endpoint,
                                cli.api_key,
                                silo_id,
                                project_id,
                                instance_id,
                                json,
                            )
                            .await
                        }
                        SiloProjectInstanceNicCommand::Get {
                            silo_id,
                            project_id,
                            instance_id,
                            nic_id,
                            json,
                        } => {
                            commands::silo_project_instance_nic_get(
                                cli.endpoint,
                                cli.api_key,
                                silo_id,
                                project_id,
                                instance_id,
                                nic_id,
                                json,
                            )
                            .await
                        }
                    },
                },
                SiloProjectCommand::Quota { command } => match command {
                    SiloProjectQuotaCommand::Set {
                        silo_id,
                        project_id,
                        cpu_limit,
                        memory_bytes,
                        disk_bytes,
                        instance_limit,
                        json,
                    } => {
                        commands::silo_project_quota_set(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            cpu_limit,
                            memory_bytes,
                            disk_bytes,
                            instance_limit,
                            json,
                        )
                        .await
                    }
                    SiloProjectQuotaCommand::Get {
                        silo_id,
                        project_id,
                        json,
                    } => {
                        commands::silo_project_quota_get(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectQuotaCommand::Delete {
                        silo_id,
                        project_id,
                    } => {
                        commands::silo_project_quota_delete(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                        )
                        .await
                    }
                },
                SiloProjectCommand::FloatingIp { command } => match command {
                    SiloProjectFloatingIpCommand::List {
                        silo_id,
                        project_id,
                        json,
                    } => {
                        commands::silo_project_floating_ip_list(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectFloatingIpCommand::Create {
                        silo_id,
                        project_id,
                        name,
                        description,
                        family,
                        json,
                    } => {
                        commands::silo_project_floating_ip_create(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            name,
                            description,
                            family,
                            json,
                        )
                        .await
                    }
                    SiloProjectFloatingIpCommand::Get {
                        silo_id,
                        project_id,
                        floating_ip_id,
                        json,
                    } => {
                        commands::silo_project_floating_ip_get(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            floating_ip_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectFloatingIpCommand::Delete {
                        silo_id,
                        project_id,
                        floating_ip_id,
                    } => {
                        commands::silo_project_floating_ip_delete(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            floating_ip_id,
                        )
                        .await
                    }
                    SiloProjectFloatingIpCommand::Attach {
                        silo_id,
                        project_id,
                        floating_ip_id,
                        nic_id,
                        json,
                    } => {
                        commands::silo_project_floating_ip_attach(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            floating_ip_id,
                            nic_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectFloatingIpCommand::Detach {
                        silo_id,
                        project_id,
                        floating_ip_id,
                        json,
                    } => {
                        commands::silo_project_floating_ip_detach(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            floating_ip_id,
                            json,
                        )
                        .await
                    }
                },
                SiloProjectCommand::Vpc { command } => match command {
                    SiloProjectVpcCommand::List {
                        silo_id,
                        project_id,
                        json,
                    } => {
                        commands::silo_project_vpc_list(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectVpcCommand::Create {
                        silo_id,
                        project_id,
                        name,
                        description,
                        ipv4_block,
                        ipv6_block,
                        json,
                    } => {
                        commands::silo_project_vpc_create(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            name,
                            description,
                            ipv4_block,
                            ipv6_block,
                            json,
                        )
                        .await
                    }
                    SiloProjectVpcCommand::Get {
                        silo_id,
                        project_id,
                        vpc_id,
                        json,
                    } => {
                        commands::silo_project_vpc_get(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            vpc_id,
                            json,
                        )
                        .await
                    }
                    SiloProjectVpcCommand::Delete {
                        silo_id,
                        project_id,
                        vpc_id,
                    } => {
                        commands::silo_project_vpc_delete(
                            cli.endpoint,
                            cli.api_key,
                            silo_id,
                            project_id,
                            vpc_id,
                        )
                        .await
                    }
                    SiloProjectVpcCommand::Subnet { command } => match command {
                        SiloProjectVpcSubnetCommand::List {
                            silo_id,
                            project_id,
                            vpc_id,
                            json,
                        } => {
                            commands::silo_project_vpc_subnet_list(
                                cli.endpoint,
                                cli.api_key,
                                silo_id,
                                project_id,
                                vpc_id,
                                json,
                            )
                            .await
                        }
                        SiloProjectVpcSubnetCommand::Create {
                            silo_id,
                            project_id,
                            vpc_id,
                            name,
                            description,
                            ipv4_block,
                            ipv6_block,
                            json,
                        } => {
                            commands::silo_project_vpc_subnet_create(
                                cli.endpoint,
                                cli.api_key,
                                silo_id,
                                project_id,
                                vpc_id,
                                name,
                                description,
                                ipv4_block,
                                ipv6_block,
                                json,
                            )
                            .await
                        }
                        SiloProjectVpcSubnetCommand::Get {
                            silo_id,
                            project_id,
                            vpc_id,
                            subnet_id,
                            json,
                        } => {
                            commands::silo_project_vpc_subnet_get(
                                cli.endpoint,
                                cli.api_key,
                                silo_id,
                                project_id,
                                vpc_id,
                                subnet_id,
                                json,
                            )
                            .await
                        }
                        SiloProjectVpcSubnetCommand::Delete {
                            silo_id,
                            project_id,
                            vpc_id,
                            subnet_id,
                        } => {
                            commands::silo_project_vpc_subnet_delete(
                                cli.endpoint,
                                cli.api_key,
                                silo_id,
                                project_id,
                                vpc_id,
                                subnet_id,
                            )
                            .await
                        }
                    },
                },
            },
            SiloCommand::SshKey { command } => match command {
                SiloSshKeyCommand::List { silo_id, json } => {
                    commands::silo_ssh_key_list(cli.endpoint, cli.api_key, silo_id, json).await
                }
                SiloSshKeyCommand::Add {
                    silo_id,
                    name,
                    description,
                    public_key,
                    public_key_file,
                    json,
                } => {
                    commands::silo_ssh_key_add(
                        cli.endpoint,
                        cli.api_key,
                        silo_id,
                        name,
                        description,
                        public_key,
                        public_key_file,
                        json,
                    )
                    .await
                }
                SiloSshKeyCommand::Get {
                    silo_id,
                    ssh_key_id,
                    json,
                } => {
                    commands::silo_ssh_key_get(cli.endpoint, cli.api_key, silo_id, ssh_key_id, json)
                        .await
                }
                SiloSshKeyCommand::Delete {
                    silo_id,
                    ssh_key_id,
                } => {
                    commands::silo_ssh_key_delete(cli.endpoint, cli.api_key, silo_id, ssh_key_id)
                        .await
                }
            },
            SiloCommand::Image { command } => match command {
                SiloImageCommand::List { silo_id, json } => {
                    commands::silo_image_list(cli.endpoint, cli.api_key, silo_id, json).await
                }
                SiloImageCommand::Add {
                    silo_id,
                    name,
                    description,
                    os,
                    version,
                    size_bytes,
                    sha256,
                    source_url,
                    id,
                    json,
                } => {
                    commands::silo_image_add(
                        cli.endpoint,
                        cli.api_key,
                        silo_id,
                        name,
                        description,
                        os,
                        version,
                        size_bytes,
                        sha256,
                        source_url,
                        id,
                        json,
                    )
                    .await
                }
                SiloImageCommand::Get {
                    silo_id,
                    image_id,
                    json,
                } => {
                    commands::silo_image_get(cli.endpoint, cli.api_key, silo_id, image_id, json)
                        .await
                }
                SiloImageCommand::Delete { silo_id, image_id } => {
                    commands::silo_image_delete(cli.endpoint, cli.api_key, silo_id, image_id).await
                }
            },
        },
    }
}
