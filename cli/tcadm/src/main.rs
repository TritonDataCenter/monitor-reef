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
    /// Manage compute-node registration and approvals.
    Cn {
        #[command(subcommand)]
        command: CnCommand,
    },
    /// Inspect and verify the audit log.
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },
    /// Manage silo-scoped resources (IdP, SSH keys, images).
    Silo {
        #[command(subcommand)]
        command: SiloCommand,
    },
    /// Manage tenant-scoped resources (projects, VPCs, instances,
    /// quotas, floating IPs). Re-parented from `silo project ...`
    /// in slice E-3.
    Tenant {
        #[command(subcommand)]
        command: TenantCommand,
    },
}

#[derive(Subcommand)]
enum SiloCommand {
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
enum TenantCommand {
    /// List tenants in a silo.
    List {
        silo_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Show a single tenant.
    Show {
        silo_id: Uuid,
        tenant_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a tenant in a silo.
    Create {
        silo_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a tenant by id.
    Delete { silo_id: Uuid, tenant_id: Uuid },
    /// Manage projects inside a tenant.
    Project {
        #[command(subcommand)]
        command: TenantProjectCommand,
    },
    /// Manage the tenant's OIDC identity provider.
    Idp {
        #[command(subcommand)]
        command: TenantIdpCommand,
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
enum TenantProjectCommand {
    /// List projects in the silo.
    List {
        tenant_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a new project.
    Create {
        tenant_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        json: bool,
    },
    /// Read a single project.
    Get {
        tenant_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a project.
    Delete { tenant_id: Uuid, project_id: Uuid },
    /// Manage VPCs inside a project.
    Vpc {
        #[command(subcommand)]
        command: TenantProjectVpcCommand,
    },
    /// Manage instances inside a project.
    Instance {
        #[command(subcommand)]
        command: TenantProjectInstanceCommand,
    },
    /// Manage the project's resource quota.
    Quota {
        #[command(subcommand)]
        command: TenantProjectQuotaCommand,
    },
    /// Manage floating IPs (project-scoped, allocated from a fleet
    /// pool, attachable to any NIC in the project).
    FloatingIp {
        #[command(subcommand)]
        command: TenantProjectFloatingIpCommand,
    },
}

#[derive(Subcommand)]
enum TenantProjectFloatingIpCommand {
    /// List floating IPs in the project.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Allocate a new floating IP from the fleet pool.
    Create {
        tenant_id: Uuid,
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
        tenant_id: Uuid,
        project_id: Uuid,
        floating_ip_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Release a floating IP back to its pool. Returns 409 if
    /// currently attached.
    Delete {
        tenant_id: Uuid,
        project_id: Uuid,
        floating_ip_id: Uuid,
    },
    /// Attach a floating IP to a NIC. Replace semantics — if the
    /// IP was already attached elsewhere, it swaps atomically.
    Attach {
        tenant_id: Uuid,
        project_id: Uuid,
        floating_ip_id: Uuid,
        #[arg(long)]
        nic_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Detach a floating IP. Idempotent.
    Detach {
        tenant_id: Uuid,
        project_id: Uuid,
        floating_ip_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TenantProjectInstanceCommand {
    /// List instances in the project.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a new instance.
    Create {
        tenant_id: Uuid,
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
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete an instance (must be Stopped or Failed).
    Delete {
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
    },
    /// Start a Stopped instance.
    Start {
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Stop a Running instance.
    Stop {
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Restart a Running instance.
    Restart {
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Inspect NICs attached to an instance.
    Nic {
        #[command(subcommand)]
        command: TenantProjectInstanceNicCommand,
    },
    /// Inspect disks attached to an instance.
    Disk {
        #[command(subcommand)]
        command: TenantProjectInstanceDiskCommand,
    },
}

#[derive(Subcommand)]
enum TenantProjectInstanceDiskCommand {
    /// List the disks attached to an instance.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read a single disk.
    Get {
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        disk_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TenantProjectInstanceNicCommand {
    /// List the NICs attached to an instance (Phase 0 ships exactly
    /// one — the auto-created `primary` NIC).
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read a single NIC.
    Get {
        tenant_id: Uuid,
        project_id: Uuid,
        instance_id: Uuid,
        nic_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TenantProjectQuotaCommand {
    /// Set (or replace) the project's quota.
    Set {
        tenant_id: Uuid,
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
        tenant_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Remove the project's quota (project becomes unlimited).
    Delete { tenant_id: Uuid, project_id: Uuid },
}

#[derive(Subcommand)]
enum TenantProjectVpcCommand {
    /// List VPCs in a project.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a new VPC in a project. At least one of `--ipv4-block`
    /// and `--ipv6-block` must be provided.
    Create {
        tenant_id: Uuid,
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
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a VPC.
    Delete {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
    },
    /// Manage subnets inside a VPC.
    Subnet {
        #[command(subcommand)]
        command: TenantProjectVpcSubnetCommand,
    },
}

#[derive(Subcommand)]
enum TenantProjectVpcSubnetCommand {
    /// List subnets in a VPC.
    List {
        tenant_id: Uuid,
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
        tenant_id: Uuid,
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
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        subnet_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a subnet.
    Delete {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        subnet_id: Uuid,
    },
}

#[derive(Subcommand)]
enum TenantIdpCommand {
    /// Configure (or replace) the tenant's IdP. Eagerly fetches
    /// the OIDC discovery document; rejects on failure. Returns
    /// 409 if a different tenant already claims the same
    /// `--issuer-url`.
    Set {
        tenant_id: Uuid,
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
    /// Read the tenant's IdP config (client secret never returned).
    Get {
        tenant_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Remove the tenant's IdP config. Federated users in that
    /// tenant will fail to authenticate until a new config is
    /// posted.
    Delete { tenant_id: Uuid },
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

#[derive(Subcommand)]
enum CnCommand {
    /// List registered compute nodes, optionally filtered by state.
    List {
        /// Filter by state. One of `pending`, `approved`, `disabled`.
        #[arg(long, value_enum)]
        state: Option<CnStateArg>,
        #[arg(long)]
        json: bool,
    },
    /// Show a single CN by server_uuid.
    Show {
        server_uuid: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Approve a Pending CN by claim code (XXX-XXX or XXXXXX).
    Approve {
        /// Six-character claim code displayed on the CN's console.
        code: String,
        #[arg(long)]
        json: bool,
    },
    /// Disable a CN; revokes the bound API key.
    Disable {
        server_uuid: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Auto-approve window controls.
    AutoApprove {
        #[command(subcommand)]
        command: AutoApproveCommand,
    },
}

#[derive(Subcommand)]
enum AutoApproveCommand {
    /// Read the current window (or null when none is open).
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Open or replace the auto-approve window.
    Open {
        /// How long to keep the window open. Server clamps to 24h.
        #[arg(long)]
        duration_secs: u64,
        /// Maximum number of registrations to auto-approve before
        /// the window closes early. Omit for time-bound only.
        #[arg(long)]
        count: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Close the window early. Idempotent.
    Close,
}

/// CLI mirror of [`tritond_client::types::CnState`]. Kept separate so
/// the clap-derived value-name uses kebab-case while the wire format
/// stays snake_case.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum CnStateArg {
    Pending,
    Approved,
    Disabled,
}

impl From<CnStateArg> for tritond_client::types::CnState {
    fn from(arg: CnStateArg) -> Self {
        match arg {
            CnStateArg::Pending => tritond_client::types::CnState::Pending,
            CnStateArg::Approved => tritond_client::types::CnState::Approved,
            CnStateArg::Disabled => tritond_client::types::CnState::Disabled,
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
        Commands::Cn { command } => match command {
            CnCommand::List { state, json } => {
                commands::cn_list(cli.endpoint, cli.api_key, state.map(Into::into), json).await
            }
            CnCommand::Show { server_uuid, json } => {
                commands::cn_show(cli.endpoint, cli.api_key, server_uuid, json).await
            }
            CnCommand::Approve { code, json } => {
                commands::cn_approve(cli.endpoint, cli.api_key, code, json).await
            }
            CnCommand::Disable { server_uuid, json } => {
                commands::cn_disable(cli.endpoint, cli.api_key, server_uuid, json).await
            }
            CnCommand::AutoApprove { command } => match command {
                AutoApproveCommand::Status { json } => {
                    commands::cn_auto_approve_status(cli.endpoint, cli.api_key, json).await
                }
                AutoApproveCommand::Open {
                    duration_secs,
                    count,
                    json,
                } => {
                    commands::cn_auto_approve_open(
                        cli.endpoint,
                        cli.api_key,
                        duration_secs,
                        count,
                        json,
                    )
                    .await
                }
                AutoApproveCommand::Close => {
                    commands::cn_auto_approve_close(cli.endpoint, cli.api_key).await
                }
            },
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
        Commands::Tenant { command } => match command {
            TenantCommand::List { silo_id, json } => {
                commands::tenant_list(cli.endpoint, cli.api_key, silo_id, json).await
            }
            TenantCommand::Show {
                silo_id,
                tenant_id,
                json,
            } => commands::tenant_show(cli.endpoint, cli.api_key, silo_id, tenant_id, json).await,
            TenantCommand::Create {
                silo_id,
                name,
                description,
                json,
            } => {
                commands::tenant_create(cli.endpoint, cli.api_key, silo_id, name, description, json)
                    .await
            }
            TenantCommand::Delete { silo_id, tenant_id } => {
                commands::tenant_delete(cli.endpoint, cli.api_key, silo_id, tenant_id).await
            }
            TenantCommand::Project { command } => match command {
                TenantProjectCommand::List { tenant_id, json } => {
                    commands::tenant_project_list(cli.endpoint, cli.api_key, tenant_id, json).await
                }
                TenantProjectCommand::Create {
                    tenant_id,
                    name,
                    description,
                    json,
                } => {
                    commands::tenant_project_create(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        name,
                        description,
                        json,
                    )
                    .await
                }
                TenantProjectCommand::Get {
                    tenant_id,
                    project_id,
                    json,
                } => {
                    commands::tenant_project_get(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        json,
                    )
                    .await
                }
                TenantProjectCommand::Delete {
                    tenant_id,
                    project_id,
                } => {
                    commands::tenant_project_delete(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                    )
                    .await
                }
                TenantProjectCommand::Instance { command } => match command {
                    TenantProjectInstanceCommand::List {
                        tenant_id,
                        project_id,
                        json,
                    } => {
                        commands::tenant_project_instance_list(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectInstanceCommand::Create {
                        tenant_id,
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
                        commands::tenant_project_instance_create(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
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
                    TenantProjectInstanceCommand::Get {
                        tenant_id,
                        project_id,
                        instance_id,
                        json,
                    } => {
                        commands::tenant_project_instance_get(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            instance_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectInstanceCommand::Delete {
                        tenant_id,
                        project_id,
                        instance_id,
                    } => {
                        commands::tenant_project_instance_delete(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            instance_id,
                        )
                        .await
                    }
                    TenantProjectInstanceCommand::Start {
                        tenant_id,
                        project_id,
                        instance_id,
                        json,
                    } => {
                        commands::tenant_project_instance_lifecycle(
                            cli.endpoint,
                            cli.api_key,
                            "start",
                            tenant_id,
                            project_id,
                            instance_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectInstanceCommand::Stop {
                        tenant_id,
                        project_id,
                        instance_id,
                        json,
                    } => {
                        commands::tenant_project_instance_lifecycle(
                            cli.endpoint,
                            cli.api_key,
                            "stop",
                            tenant_id,
                            project_id,
                            instance_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectInstanceCommand::Restart {
                        tenant_id,
                        project_id,
                        instance_id,
                        json,
                    } => {
                        commands::tenant_project_instance_lifecycle(
                            cli.endpoint,
                            cli.api_key,
                            "restart",
                            tenant_id,
                            project_id,
                            instance_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectInstanceCommand::Disk { command } => match command {
                        TenantProjectInstanceDiskCommand::List {
                            tenant_id,
                            project_id,
                            instance_id,
                            json,
                        } => {
                            commands::tenant_project_instance_disk_list(
                                cli.endpoint,
                                cli.api_key,
                                tenant_id,
                                project_id,
                                instance_id,
                                json,
                            )
                            .await
                        }
                        TenantProjectInstanceDiskCommand::Get {
                            tenant_id,
                            project_id,
                            instance_id,
                            disk_id,
                            json,
                        } => {
                            commands::tenant_project_instance_disk_get(
                                cli.endpoint,
                                cli.api_key,
                                tenant_id,
                                project_id,
                                instance_id,
                                disk_id,
                                json,
                            )
                            .await
                        }
                    },
                    TenantProjectInstanceCommand::Nic { command } => match command {
                        TenantProjectInstanceNicCommand::List {
                            tenant_id,
                            project_id,
                            instance_id,
                            json,
                        } => {
                            commands::tenant_project_instance_nic_list(
                                cli.endpoint,
                                cli.api_key,
                                tenant_id,
                                project_id,
                                instance_id,
                                json,
                            )
                            .await
                        }
                        TenantProjectInstanceNicCommand::Get {
                            tenant_id,
                            project_id,
                            instance_id,
                            nic_id,
                            json,
                        } => {
                            commands::tenant_project_instance_nic_get(
                                cli.endpoint,
                                cli.api_key,
                                tenant_id,
                                project_id,
                                instance_id,
                                nic_id,
                                json,
                            )
                            .await
                        }
                    },
                },
                TenantProjectCommand::Quota { command } => match command {
                    TenantProjectQuotaCommand::Set {
                        tenant_id,
                        project_id,
                        cpu_limit,
                        memory_bytes,
                        disk_bytes,
                        instance_limit,
                        json,
                    } => {
                        commands::tenant_project_quota_set(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            cpu_limit,
                            memory_bytes,
                            disk_bytes,
                            instance_limit,
                            json,
                        )
                        .await
                    }
                    TenantProjectQuotaCommand::Get {
                        tenant_id,
                        project_id,
                        json,
                    } => {
                        commands::tenant_project_quota_get(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectQuotaCommand::Delete {
                        tenant_id,
                        project_id,
                    } => {
                        commands::tenant_project_quota_delete(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                        )
                        .await
                    }
                },
                TenantProjectCommand::FloatingIp { command } => match command {
                    TenantProjectFloatingIpCommand::List {
                        tenant_id,
                        project_id,
                        json,
                    } => {
                        commands::tenant_project_floating_ip_list(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectFloatingIpCommand::Create {
                        tenant_id,
                        project_id,
                        name,
                        description,
                        family,
                        json,
                    } => {
                        commands::tenant_project_floating_ip_create(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            name,
                            description,
                            family,
                            json,
                        )
                        .await
                    }
                    TenantProjectFloatingIpCommand::Get {
                        tenant_id,
                        project_id,
                        floating_ip_id,
                        json,
                    } => {
                        commands::tenant_project_floating_ip_get(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            floating_ip_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectFloatingIpCommand::Delete {
                        tenant_id,
                        project_id,
                        floating_ip_id,
                    } => {
                        commands::tenant_project_floating_ip_delete(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            floating_ip_id,
                        )
                        .await
                    }
                    TenantProjectFloatingIpCommand::Attach {
                        tenant_id,
                        project_id,
                        floating_ip_id,
                        nic_id,
                        json,
                    } => {
                        commands::tenant_project_floating_ip_attach(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            floating_ip_id,
                            nic_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectFloatingIpCommand::Detach {
                        tenant_id,
                        project_id,
                        floating_ip_id,
                        json,
                    } => {
                        commands::tenant_project_floating_ip_detach(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            floating_ip_id,
                            json,
                        )
                        .await
                    }
                },
                TenantProjectCommand::Vpc { command } => match command {
                    TenantProjectVpcCommand::List {
                        tenant_id,
                        project_id,
                        json,
                    } => {
                        commands::tenant_project_vpc_list(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectVpcCommand::Create {
                        tenant_id,
                        project_id,
                        name,
                        description,
                        ipv4_block,
                        ipv6_block,
                        json,
                    } => {
                        commands::tenant_project_vpc_create(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            name,
                            description,
                            ipv4_block,
                            ipv6_block,
                            json,
                        )
                        .await
                    }
                    TenantProjectVpcCommand::Get {
                        tenant_id,
                        project_id,
                        vpc_id,
                        json,
                    } => {
                        commands::tenant_project_vpc_get(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectVpcCommand::Delete {
                        tenant_id,
                        project_id,
                        vpc_id,
                    } => {
                        commands::tenant_project_vpc_delete(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                        )
                        .await
                    }
                    TenantProjectVpcCommand::Subnet { command } => match command {
                        TenantProjectVpcSubnetCommand::List {
                            tenant_id,
                            project_id,
                            vpc_id,
                            json,
                        } => {
                            commands::tenant_project_vpc_subnet_list(
                                cli.endpoint,
                                cli.api_key,
                                tenant_id,
                                project_id,
                                vpc_id,
                                json,
                            )
                            .await
                        }
                        TenantProjectVpcSubnetCommand::Create {
                            tenant_id,
                            project_id,
                            vpc_id,
                            name,
                            description,
                            ipv4_block,
                            ipv6_block,
                            json,
                        } => {
                            commands::tenant_project_vpc_subnet_create(
                                cli.endpoint,
                                cli.api_key,
                                tenant_id,
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
                        TenantProjectVpcSubnetCommand::Get {
                            tenant_id,
                            project_id,
                            vpc_id,
                            subnet_id,
                            json,
                        } => {
                            commands::tenant_project_vpc_subnet_get(
                                cli.endpoint,
                                cli.api_key,
                                tenant_id,
                                project_id,
                                vpc_id,
                                subnet_id,
                                json,
                            )
                            .await
                        }
                        TenantProjectVpcSubnetCommand::Delete {
                            tenant_id,
                            project_id,
                            vpc_id,
                            subnet_id,
                        } => {
                            commands::tenant_project_vpc_subnet_delete(
                                cli.endpoint,
                                cli.api_key,
                                tenant_id,
                                project_id,
                                vpc_id,
                                subnet_id,
                            )
                            .await
                        }
                    },
                },
            },
            TenantCommand::Idp { command } => match command {
                TenantIdpCommand::Set {
                    tenant_id,
                    issuer_url,
                    client_id,
                    client_secret_stdin,
                    audience,
                    json,
                } => {
                    commands::tenant_idp_set(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        issuer_url,
                        client_id,
                        client_secret_stdin,
                        audience,
                        json,
                    )
                    .await
                }
                TenantIdpCommand::Get { tenant_id, json } => {
                    commands::tenant_idp_get(cli.endpoint, cli.api_key, tenant_id, json).await
                }
                TenantIdpCommand::Delete { tenant_id } => {
                    commands::tenant_idp_delete(cli.endpoint, cli.api_key, tenant_id).await
                }
            },
        },
    }
}
