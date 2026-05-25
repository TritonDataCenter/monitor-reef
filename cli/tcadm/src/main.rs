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
mod http;
mod install;
mod self_update;
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
    /// RFD 00007 flat-verb tree for instances. Calls the new /v1/
    /// surface; the legacy `tcadm tenant project instance list ...`
    /// nested verbs stay around through AP-3e for backwards-compat
    /// during the cutover.
    Instance {
        #[command(subcommand)]
        command: InstanceCommand,
    },
    /// RFD 00007 fleet-admin operator commands. Capability-gated;
    /// callers without the right `Capability` see 404 NotFound.
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },
    /// Inspect legacy (non-tritond-managed) zones discovered by the
    /// classifier on registered CNs. Fleet-admin only.
    Legacy {
        #[command(subcommand)]
        command: LegacyCommand,
    },
    /// Inspect and verify the audit log.
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },
    /// Inspect long-running operations (durable workflow runs / sagas).
    /// See RFD 00004.
    Operations {
        #[command(subcommand)]
        command: OperationsCommand,
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
    /// Manage network resources with operational shorthand commands.
    Net {
        #[command(subcommand)]
        command: NetCommand,
    },
    /// Manage Public images (operator-facing root commands).
    /// Tenant- / project- / user-scoped images live under the
    /// `tenant`, `tenant project`, and `auth` subtrees.
    Image {
        #[command(subcommand)]
        command: PublicImageCommand,
    },
    /// Manage Public SSH keys (operator-facing root commands)
    /// plus the global show/delete-by-id endpoints.
    /// Tenant- / project- / user-scoped keys live under the
    /// `tenant`, `tenant project`, and `auth` subtrees.
    SshKey {
        #[command(subcommand)]
        command: PublicSshKeyCommand,
    },
    /// Caller-scoped resources (your own user-scoped images and
    /// ssh keys).
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Manage layered IMDS metadata at any of the four scopes
    /// (silo / tenant / project / instance), plus the realized view
    /// for one instance. See `IMDS_DESIGN.md` §4.1.
    Meta {
        #[command(subcommand)]
        command: MetaCommand,
    },
    /// Manage registered manta-storage clusters (operator-only).
    /// Forwarder endpoints (buckets / users / policies) are exposed
    /// through admin-backend; tcadm stays at the registry level for now.
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
    /// View and change cluster-wide tritond settings (fleet-admin
    /// only). The minimum tritond needs to start lives in its
    /// bootstrap config file; everything here lives in FoundationDB
    /// and takes effect on the next tritond restart.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Install an image or agent from the signed Manta release
    /// channel onto this host. Auto-detects whether `<name>` is a
    /// zone image (drives imgadm install) or a GZ agent tarball
    /// (extracts at /, imports SMF, enables service). With `--list`,
    /// enumerates channel contents alongside installed status.
    Install {
        /// Name of the image or agent (e.g. `triton-fdb`,
        /// `tritonagent`). Required unless `--list` is given.
        name: Option<String>,
        /// Pin to a specific stamp. Channel must already point at
        /// this stamp; we refuse to download arbitrary stamps the
        /// channel doesn't currently advertise.
        #[arg(long)]
        stamp: Option<String>,
        /// Override the channel manifest URL.
        #[arg(long)]
        channel_url: Option<String>,
        /// List channel contents + installed status; do not install.
        #[arg(long)]
        list: bool,
    },

    /// Update this `tcadm` binary against the signed Manta release
    /// channel. Verifies the channel signature against the publisher
    /// pubkey baked into the binary; refuses to act on a tampered or
    /// mis-signed manifest. Atomic swap; the previous binary is kept
    /// at `tcadm.prev` for manual rollback.
    SelfUpdate {
        /// Override the channel manifest URL. Defaults to the stable
        /// channel under tritoncloud.
        #[arg(long)]
        channel_url: Option<String>,
        /// Override the install directory. Defaults to the directory
        /// of the currently-running binary.
        #[arg(long)]
        install_dir: Option<std::path::PathBuf>,
        /// Report current vs latest only; do not download or replace.
        /// Exits non-zero if an update is available.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// List every configuration key with its value, default,
    /// description, and any environment variable overriding it.
    List {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show one configuration key.
    Get {
        /// Dotted key name, e.g. `sweeper.interval_secs`.
        key: String,
        /// Emit JSON instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
    /// Set one configuration key. The value is parsed as JSON when it
    /// looks like JSON (`30`, `true`, `null`); otherwise it is taken
    /// as a string (`clickhouse`, `http://ch:8123`).
    Set {
        /// Dotted key name, e.g. `metrics.backend`.
        key: String,
        /// New value.
        value: String,
    },
    /// Reset one configuration key to its built-in default.
    Reset {
        /// Dotted key name.
        key: String,
    },
}

#[derive(Subcommand)]
enum StorageCommand {
    /// Manage cluster registrations.
    Cluster {
        #[command(subcommand)]
        command: StorageClusterCommand,
    },
}

#[derive(Subcommand)]
enum StorageClusterCommand {
    /// List every registered storage cluster, sorted by name.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Read a single cluster by id or by name.
    Show {
        /// UUID or operator-chosen name.
        ident: String,
        #[arg(long)]
        json: bool,
    },
    /// Register a new cluster. Surface defaults to `s3` because
    /// that's the only forwarder family wired up today (mantafs /
    /// manta-block registrations succeed but their forwarder
    /// endpoints return 409).
    Add {
        /// Operator-chosen short name. Unique cluster-wide.
        #[arg(long)]
        name: String,
        /// HTTP base URL of the cluster's admin API
        /// (e.g. http://10.199.199.250:7101). Distinct from the
        /// global `--endpoint` flag, which controls the *tritond*
        /// URL tcadm itself talks to.
        #[arg(long = "cluster-endpoint")]
        cluster_endpoint: String,
        /// Bearer token tritond will present on /admin/v1/* calls.
        /// Read from `--admin-token-stdin` (one line, trimmed) when
        /// not passed inline so the secret doesn't end up in shell
        /// history.
        #[arg(long, conflicts_with = "admin_token_stdin")]
        admin_token: Option<String>,
        #[arg(long, conflicts_with = "admin_token")]
        admin_token_stdin: bool,
        /// Surface served by this cluster.
        #[arg(long, value_enum, default_value_t = StorageSurfaceArg::S3)]
        surface: StorageSurfaceArg,
        /// Default region echoed back to clients (informational).
        #[arg(long, default_value = "us-east-1")]
        default_region: String,
        /// Optional human-friendly label.
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Deregister a cluster. Idempotent; succeeds when the id is
    /// already gone.
    Delete {
        /// Cluster UUID.
        cluster_id: Uuid,
    },
    /// Trigger an out-of-band health probe and persist the result.
    Health {
        /// UUID or operator-chosen name.
        ident: String,
        #[arg(long)]
        json: bool,
    },
    /// Configure (or rotate) the IAM presigner credential tritond
    /// signs S3 presigned URLs with. Required before the bucket
    /// browser can mint upload/download URLs.
    SetPresigner {
        /// UUID or operator-chosen name.
        ident: String,
        /// HTTP base URL of the cluster's S3 data plane
        /// (e.g. https://10.199.199.250:7443). Distinct from
        /// the admin URL on port 7101. Optional on rotations of
        /// the credential alone.
        #[arg(long)]
        s3_endpoint: Option<String>,
        /// IAM access key id (e.g. AKIA...).
        #[arg(long)]
        access_key_id: String,
        /// Secret access key. Required, but please pass it via
        /// `--secret-access-key-stdin` so the secret doesn't end
        /// up in shell history.
        #[arg(long, conflicts_with = "secret_access_key_stdin")]
        secret_access_key: Option<String>,
        #[arg(long, conflicts_with = "secret_access_key")]
        secret_access_key_stdin: bool,
        #[arg(long)]
        json: bool,
    },
    /// Drop the presigner credential. The bucket browser will
    /// stop being able to mint upload/download URLs against this
    /// cluster until a new presigner is configured.
    ClearPresigner {
        /// UUID or operator-chosen name.
        ident: String,
    },
}

/// CLI-side surface enum. Maps to `tritond_client::types::StorageClusterSurface`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum StorageSurfaceArg {
    S3,
    Fs,
    Block,
}

#[derive(Subcommand)]
enum PublicImageCommand {
    /// List Public images. Anonymous-accessible.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Register a new Public image. Root-only.
    Add {
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
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        source_url: Option<String>,
        #[arg(long)]
        id: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single image by id (visibility-filtered server-side).
    Get {
        image_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete an image by id (ownership-gated server-side).
    Delete { image_id: Uuid },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Manage your own (caller-scoped) images.
    Image {
        #[command(subcommand)]
        command: AuthImageCommand,
    },
    /// Manage your own (caller-scoped) SSH keys.
    SshKey {
        #[command(subcommand)]
        command: AuthSshKeyCommand,
    },
}

#[derive(Subcommand)]
enum AuthImageCommand {
    /// List your `User`-scoped images.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Register a new `User`-scoped image owned by the caller.
    Add {
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
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        source_url: Option<String>,
        #[arg(long)]
        id: Option<Uuid>,
        #[arg(long)]
        json: bool,
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
    /// Manage `Tenant`-scoped images.
    Image {
        #[command(subcommand)]
        command: TenantImageCommand,
    },
    /// Manage `Tenant`-scoped SSH keys.
    SshKey {
        #[command(subcommand)]
        command: TenantSshKeyCommand,
    },
}

#[derive(Subcommand)]
enum TenantImageCommand {
    /// List images visible to this tenant (Public + Silo + Tenant).
    List {
        tenant_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Register a new `Tenant`-scoped image.
    Add {
        tenant_id: Uuid,
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
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        source_url: Option<String>,
        #[arg(long)]
        id: Option<Uuid>,
        #[arg(long)]
        json: bool,
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
    /// List `Silo`-scoped SSH keys (does NOT include Public; use
    /// `tcadm tenant ssh-key list` for the unioned tenant view).
    List {
        silo_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Register a new `Silo`-scoped SSH key. Reads the openssh
    /// public-key string from `--public-key-file` (one line) or
    /// `--public-key`.
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
}

#[derive(Subcommand)]
enum PublicSshKeyCommand {
    /// List Public SSH keys. Anonymous-accessible.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Register a new Public SSH key. Root-only.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, conflicts_with = "public_key_file")]
        public_key: Option<String>,
        #[arg(long, conflicts_with = "public_key")]
        public_key_file: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single SSH key by id (visibility-filtered server-side).
    Show {
        key_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete an SSH key by id (ownership-gated server-side).
    Delete { key_id: Uuid },
}

#[derive(Subcommand)]
enum TenantSshKeyCommand {
    /// List SSH keys visible to this tenant (Public + Silo + Tenant).
    List {
        tenant_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Register a new `Tenant`-scoped SSH key.
    Add {
        tenant_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, conflicts_with = "public_key_file")]
        public_key: Option<String>,
        #[arg(long, conflicts_with = "public_key")]
        public_key_file: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TenantProjectSshKeyCommand {
    /// List SSH keys visible to this project (Public + Silo +
    /// Tenant + Project).
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Register a new `Project`-scoped SSH key.
    Add {
        tenant_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, conflicts_with = "public_key_file")]
        public_key: Option<String>,
        #[arg(long, conflicts_with = "public_key")]
        public_key_file: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum AuthSshKeyCommand {
    /// List your `User`-scoped SSH keys.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Register a new `User`-scoped SSH key owned by the caller.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, conflicts_with = "public_key_file")]
        public_key: Option<String>,
        #[arg(long, conflicts_with = "public_key")]
        public_key_file: Option<String>,
        #[arg(long)]
        json: bool,
    },
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
    /// Manage `Project`-scoped images.
    Image {
        #[command(subcommand)]
        command: TenantProjectImageCommand,
    },
    /// Manage `Project`-scoped SSH keys.
    SshKey {
        #[command(subcommand)]
        command: TenantProjectSshKeyCommand,
    },
}

#[derive(Subcommand)]
enum TenantProjectImageCommand {
    /// List images visible to this project (Public + Silo +
    /// Tenant + Project).
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Register a new `Project`-scoped image.
    Add {
        tenant_id: Uuid,
        project_id: Uuid,
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
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        source_url: Option<String>,
        #[arg(long)]
        id: Option<Uuid>,
        #[arg(long)]
        json: bool,
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
enum NetCommand {
    /// Manage VPC route tables.
    RouteTable {
        #[command(subcommand)]
        command: NetRouteTableCommand,
    },
    /// Manage VPC routes.
    Route {
        #[command(subcommand)]
        command: NetRouteCommand,
    },
    /// Manage VPC NAT gateways.
    NatGw {
        #[command(subcommand)]
        command: NetNatGwCommand,
    },
    /// Manage VPC DHCP/IPAM (pool config, reservations, leases).
    Dhcp {
        #[command(subcommand)]
        command: NetDhcpCommand,
    },
}

#[derive(Subcommand)]
enum NetDhcpCommand {
    /// Manage the per-VPC DHCP pool (lease cadence, exclusions, raw options).
    Pool {
        #[command(subcommand)]
        command: NetDhcpPoolCommand,
    },
    /// Manage sticky MAC -> IP reservations.
    Reservation {
        #[command(subcommand)]
        command: NetDhcpReservationCommand,
    },
    /// Inspect or release issued DHCP leases.
    Lease {
        #[command(subcommand)]
        command: NetDhcpLeaseCommand,
    },
}

#[derive(Subcommand)]
enum NetDhcpPoolCommand {
    /// Show the VPC's DHCP pool config (prints "(none)" when unset).
    Show {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Set the VPC's DHCP pool config (replaces any existing config).
    Set {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        /// DHCP option-51 renewal-cadence hint, in seconds.
        #[arg(long, default_value_t = 86_400)]
        lease_seconds: u32,
        /// IPv4 address to exclude from allocation. Repeatable.
        #[arg(long = "exclude", value_name = "IPV4")]
        exclude: Vec<std::net::Ipv4Addr>,
        /// Extra raw DHCP option as CODE=HEXBYTES (e.g. 42=c63364fa).
        /// Repeatable.
        #[arg(long = "option", value_name = "CODE=HEX")]
        option: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Clear the VPC's DHCP pool config (revert to subnet defaults).
    Clear {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
    },
}

#[derive(Subcommand)]
enum NetDhcpReservationCommand {
    /// List the VPC's reservations.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Add (or replace) a sticky MAC -> IP reservation.
    Add {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        mac: String,
        #[arg(long, value_name = "IPV4")]
        ip: std::net::Ipv4Addr,
        #[arg(long)]
        hostname: Option<String>,
        /// Per-MAC raw DHCP option as CODE=HEXBYTES (e.g. 252=687474703a2f2f...).
        /// Repeatable.
        #[arg(long = "option", value_name = "CODE=HEX")]
        option: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show a single reservation by MAC.
    Get {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        mac: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove a reservation by MAC.
    Remove {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        mac: String,
    },
}

#[derive(Subcommand)]
enum NetDhcpLeaseCommand {
    /// List the VPC's issued leases.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Show a single lease by MAC.
    Get {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        mac: String,
        #[arg(long)]
        json: bool,
    },
    /// Release (delete) a lease by MAC. Frees the IP; breaks
    /// sticky-by-MAC for that MAC until a reservation is re-created.
    Release {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        mac: String,
    },
}

#[derive(Subcommand)]
enum NetRouteTableCommand {
    /// List route tables in a VPC.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a route table in a VPC.
    Create {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        json: bool,
    },
    /// Read a single route table.
    Get {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        route_table_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a route table.
    Delete {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        route_table_id: Uuid,
    },
}

#[derive(Subcommand)]
enum NetRouteCommand {
    /// List routes in a route table.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        route_table_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a route in a route table.
    Create {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        route_table_id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// Destination CIDR, e.g. 0.0.0.0/0.
        #[arg(long)]
        destination: String,
        /// Target: blackhole, reject, virtual-gateway, nat-gateway:<uuid>, or floating-ip:<uuid>.
        #[arg(long)]
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Read a single route.
    Get {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        route_table_id: Uuid,
        route_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a route.
    Delete {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        route_table_id: Uuid,
        route_id: Uuid,
    },
}

#[derive(Subcommand)]
enum NetNatGwCommand {
    /// List NAT gateways in a VPC.
    List {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a NAT gateway in a VPC.
    Create {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
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
    /// Read a single NAT gateway.
    Get {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        nat_gateway_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Delete a NAT gateway and release its public address.
    Delete {
        tenant_id: Uuid,
        project_id: Uuid,
        vpc_id: Uuid,
        nat_gateway_id: Uuid,
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
enum OperationsCommand {
    /// List operations (durable workflow runs).
    List {
        /// Return operations strictly after this id.
        #[arg(long)]
        after_id: Option<Uuid>,
        /// Maximum operations to return (default 50, max 200).
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Fetch one operation by id, including its persisted DAG.
    Get {
        operation_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Operator-initiated unwind (RFD 00004 D-Sg-12): inject an
    /// error at every pending saga node so the next one fails and
    /// the catalog's own undos run. The currently-running action
    /// (if any) completes its natural outcome first.
    Abandon {
        operation_id: Uuid,
        #[arg(long)]
        json: bool,
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

/// Scope arg for `tcadm meta ...`. Matches the four `MetaScope`
/// values from `tritond-store` / `tritond-api`; converted to the
/// progenitor type via `From` at the call site.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum MetaScopeArg {
    Silo,
    Tenant,
    Project,
    Instance,
}

impl From<MetaScopeArg> for tritond_client::types::MetaScope {
    fn from(s: MetaScopeArg) -> Self {
        match s {
            MetaScopeArg::Silo => tritond_client::types::MetaScope::Silo,
            MetaScopeArg::Tenant => tritond_client::types::MetaScope::Tenant,
            MetaScopeArg::Project => tritond_client::types::MetaScope::Project,
            MetaScopeArg::Instance => tritond_client::types::MetaScope::Instance,
        }
    }
}

#[derive(Subcommand)]
enum MetaCommand {
    /// List every metadata entry at one scope.
    List {
        #[arg(long, value_enum)]
        scope: MetaScopeArg,
        /// UUID of the owning entity (silo / tenant / project /
        /// instance).
        #[arg(long)]
        id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read one metadata entry by key.
    Get {
        #[arg(long, value_enum)]
        scope: MetaScopeArg,
        #[arg(long)]
        id: Uuid,
        /// Metadata key, e.g. `config/ntp-servers`,
        /// `state/active-color`, `instance/role`, `guest/leader`,
        /// `user-data`. Slash-separated; see `IMDS_DESIGN.md` §1.3.
        #[arg(long)]
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Upsert one metadata entry.
    Set {
        #[arg(long, value_enum)]
        scope: MetaScopeArg,
        #[arg(long)]
        id: Uuid,
        #[arg(long)]
        key: String,
        /// Value as a JSON literal (e.g. `'"10.0.0.2"'`, `'42'`,
        /// `'{"a":1}'`). A bare string without quotes is also
        /// accepted and stored as a JSON string.
        #[arg(long)]
        value: String,
        /// Override the default guest-visibility (defaults follow
        /// `tritond-store::default_guest_visible`: true for
        /// `config/*` and `state/*` at every scope, true at
        /// project/instance for other prefixes, false at
        /// silo/tenant for other prefixes).
        #[arg(long)]
        guest_visible: Option<bool>,
        /// Mark this key guest-writable. Only meaningful on
        /// `guest/*` keys at instance scope; the server rejects it
        /// elsewhere.
        #[arg(long)]
        guest_writable: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete one metadata entry.
    Unset {
        #[arg(long, value_enum)]
        scope: MetaScopeArg,
        #[arg(long)]
        id: Uuid,
        #[arg(long)]
        key: String,
    },
    /// The full realized view for one instance: the precedence
    /// merge of silo/tenant/project/instance metadata plus the
    /// computed system keys, each leaf tagged with its provenance
    /// scope.
    Realized {
        /// Instance UUID.
        #[arg(long)]
        instance: Uuid,
        #[arg(long)]
        json: bool,
    },
}

/// RFD 00007 AP-3c-2: flat-verb instance commands. Calls the new
/// `/v1/instances` and friends. Today this complements the legacy
/// `tcadm tenant project instance list ...` nested verb; the
/// legacy form deletes at AP-3e along with the v2 server paths.
#[derive(Subcommand)]
enum InstanceCommand {
    /// List instances. Selectors:
    ///   --image=<uuid>      indexed (AP-1c)
    ///   --cn=<uuid>         indexed
    ///   --tenant=<uuid> --project=<uuid>
    ///                       bounded by project-membership index
    ///   --state=<running|stopped|...>
    ///                       narrows the result set client-side
    /// One of (image, cn, tenant+project) is required.
    List {
        #[arg(long)]
        tenant: Option<Uuid>,
        #[arg(long)]
        project: Option<Uuid>,
        #[arg(long)]
        image: Option<Uuid>,
        #[arg(long)]
        cn: Option<Uuid>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single instance by UUID.
    Show {
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

/// RFD 00007 AP-3c-2: fleet-admin operator commands. All under
/// `/v1/system/`; capability-gated server-side. A caller without
/// the right `Capability` sees the same 404 as a missing resource.
#[derive(Subcommand)]
enum SystemCommand {
    /// Fleet-wide instance search. The answer to "which VMs use
    /// image X?" and "what is on CN Y?". Capability: `SystemRead`.
    Instances {
        #[arg(long)]
        image: Option<Uuid>,
        #[arg(long)]
        cn: Option<Uuid>,
        #[arg(long)]
        silo: Option<Uuid>,
        #[arg(long)]
        tenant: Option<Uuid>,
        #[arg(long)]
        project: Option<Uuid>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Fleet-wide NIC search ("who owns 10.x.x.x?").
    Nics {
        #[arg(long)]
        ip: Option<std::net::IpAddr>,
        #[arg(long)]
        subnet: Option<Uuid>,
        #[arg(long)]
        instance: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    /// Fleet CN inventory.
    Cns {
        #[arg(long, value_enum)]
        state: Option<CnStateArg>,
        #[arg(long)]
        json: bool,
    },
    /// Grant a capability to a user.
    UserGrant {
        user_id: Uuid,
        /// Capability to grant: system-read, system-operate,
        /// system-config-write, or storage-admin.
        capability: String,
    },
    /// Revoke a capability from a user.
    UserRevoke {
        user_id: Uuid,
        capability: String,
    },
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
    /// Operator-controlled CN labels.
    Label {
        #[command(subcommand)]
        command: CnLabelCommand,
    },
    /// Auto-approve window controls.
    AutoApprove {
        #[command(subcommand)]
        command: AutoApproveCommand,
    },
}

#[derive(Subcommand)]
enum LegacyCommand {
    /// List CNs with their managed-vs-legacy zone counts.
    Cns {
        #[arg(long)]
        json: bool,
    },
    /// List legacy zones across the fleet, optionally filtered by host CN.
    Vms {
        /// Restrict to legacy zones hosted on the given CN.
        #[arg(long)]
        host_cn: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    /// Show one legacy zone's full record (including NIC inventory).
    Show {
        /// SmartOS zone uuid.
        smartos_uuid: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CnLabelCommand {
    /// Set placement labels used by the vnext placers.
    Set {
        server_uuid: Uuid,
        /// Placement role. `tenant` is default; `edge` is eligible
        /// for firehyve/fhrun north/south edge instances.
        #[arg(long, value_enum)]
        role: CnRoleArg,
        #[arg(long)]
        json: bool,
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

/// CLI mirror of [`tritond_client::types::CnRole`].
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum CnRoleArg {
    Tenant,
    Edge,
    Both,
}

impl From<CnRoleArg> for tritond_client::types::CnRole {
    fn from(arg: CnRoleArg) -> Self {
        match arg {
            CnRoleArg::Tenant => tritond_client::types::CnRole::Tenant,
            CnRoleArg::Edge => tritond_client::types::CnRole::Edge,
            CnRoleArg::Both => tritond_client::types::CnRole::Both,
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

    // rustls 0.23 wants a process-default CryptoProvider before the
    // first ClientConfig::builder() call. SmartOS GZ has no system
    // CA bundle, so the platform verifier path is unusable; we ship
    // webpki-roots in `session::build_http_client`. Installing
    // aws-lc-rs as the default is harmless if already installed.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

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
        Commands::Instance { command } => match command {
            InstanceCommand::List {
                tenant,
                project,
                image,
                cn,
                state,
                json,
            } => {
                commands::instance_list_v1(
                    cli.endpoint,
                    cli.api_key,
                    tenant,
                    project,
                    image,
                    cn,
                    state,
                    json,
                )
                .await
            }
            InstanceCommand::Show { instance_id, json } => {
                commands::instance_show_v1(cli.endpoint, cli.api_key, instance_id, json).await
            }
        },
        Commands::System { command } => match command {
            SystemCommand::Instances {
                image,
                cn,
                silo,
                tenant,
                project,
                state,
                json,
            } => {
                commands::system_instances_v1(
                    cli.endpoint,
                    cli.api_key,
                    image,
                    cn,
                    silo,
                    tenant,
                    project,
                    state,
                    json,
                )
                .await
            }
            SystemCommand::Nics {
                ip,
                subnet,
                instance,
                json,
            } => {
                commands::system_nics_v1(
                    cli.endpoint,
                    cli.api_key,
                    ip,
                    subnet,
                    instance,
                    json,
                )
                .await
            }
            SystemCommand::Cns { state, json } => {
                commands::system_cns_v1(
                    cli.endpoint,
                    cli.api_key,
                    state.map(Into::into),
                    json,
                )
                .await
            }
            SystemCommand::UserGrant {
                user_id,
                capability,
            } => commands::system_user_grant_v1(cli.endpoint, cli.api_key, user_id, capability).await,
            SystemCommand::UserRevoke {
                user_id,
                capability,
            } => commands::system_user_revoke_v1(cli.endpoint, cli.api_key, user_id, capability).await,
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
            CnCommand::Label { command } => match command {
                CnLabelCommand::Set {
                    server_uuid,
                    role,
                    json,
                } => {
                    commands::cn_label_set(
                        cli.endpoint,
                        cli.api_key,
                        server_uuid,
                        role.into(),
                        json,
                    )
                    .await
                }
            },
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
        Commands::Legacy { command } => match command {
            LegacyCommand::Cns { json } => {
                commands::legacy_cn_list(cli.endpoint, cli.api_key, json).await
            }
            LegacyCommand::Vms { host_cn, json } => {
                commands::legacy_vm_list(cli.endpoint, cli.api_key, host_cn, json).await
            }
            LegacyCommand::Show { smartos_uuid, json } => {
                commands::legacy_vm_show(cli.endpoint, cli.api_key, smartos_uuid, json).await
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
        Commands::Operations { command } => match command {
            OperationsCommand::List {
                after_id,
                limit,
                json,
            } => commands::operations_list(cli.endpoint, cli.api_key, after_id, limit, json).await,
            OperationsCommand::Get { operation_id, json } => {
                commands::operations_get(cli.endpoint, cli.api_key, operation_id, json).await
            }
            OperationsCommand::Abandon { operation_id, json } => {
                commands::operations_abandon(cli.endpoint, cli.api_key, operation_id, json).await
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
                TenantProjectCommand::Image { command } => match command {
                    TenantProjectImageCommand::List {
                        tenant_id,
                        project_id,
                        json,
                    } => {
                        commands::project_image_list(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectImageCommand::Add {
                        tenant_id,
                        project_id,
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
                        commands::project_image_add(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
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
                },
                TenantProjectCommand::SshKey { command } => match command {
                    TenantProjectSshKeyCommand::List {
                        tenant_id,
                        project_id,
                        json,
                    } => {
                        commands::project_ssh_key_list(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            json,
                        )
                        .await
                    }
                    TenantProjectSshKeyCommand::Add {
                        tenant_id,
                        project_id,
                        name,
                        description,
                        public_key,
                        public_key_file,
                        json,
                    } => {
                        commands::project_ssh_key_add(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            name,
                            description,
                            public_key,
                            public_key_file,
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
            TenantCommand::Image { command } => match command {
                TenantImageCommand::List { tenant_id, json } => {
                    commands::tenant_image_list(cli.endpoint, cli.api_key, tenant_id, json).await
                }
                TenantImageCommand::Add {
                    tenant_id,
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
                    commands::tenant_image_add(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
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
            },
            TenantCommand::SshKey { command } => match command {
                TenantSshKeyCommand::List { tenant_id, json } => {
                    commands::tenant_ssh_key_list(cli.endpoint, cli.api_key, tenant_id, json).await
                }
                TenantSshKeyCommand::Add {
                    tenant_id,
                    name,
                    description,
                    public_key,
                    public_key_file,
                    json,
                } => {
                    commands::tenant_ssh_key_add(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        name,
                        description,
                        public_key,
                        public_key_file,
                        json,
                    )
                    .await
                }
            },
        },
        Commands::Net { command } => match command {
            NetCommand::RouteTable { command } => match command {
                NetRouteTableCommand::List {
                    tenant_id,
                    project_id,
                    vpc_id,
                    json,
                } => {
                    commands::net_route_table_list(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        json,
                    )
                    .await
                }
                NetRouteTableCommand::Create {
                    tenant_id,
                    project_id,
                    vpc_id,
                    name,
                    description,
                    json,
                } => {
                    commands::net_route_table_create(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        name,
                        description,
                        json,
                    )
                    .await
                }
                NetRouteTableCommand::Get {
                    tenant_id,
                    project_id,
                    vpc_id,
                    route_table_id,
                    json,
                } => {
                    commands::net_route_table_get(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        route_table_id,
                        json,
                    )
                    .await
                }
                NetRouteTableCommand::Delete {
                    tenant_id,
                    project_id,
                    vpc_id,
                    route_table_id,
                } => {
                    commands::net_route_table_delete(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        route_table_id,
                    )
                    .await
                }
            },
            NetCommand::Route { command } => match command {
                NetRouteCommand::List {
                    tenant_id,
                    project_id,
                    vpc_id,
                    route_table_id,
                    json,
                } => {
                    commands::net_route_list(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        route_table_id,
                        json,
                    )
                    .await
                }
                NetRouteCommand::Create {
                    tenant_id,
                    project_id,
                    vpc_id,
                    route_table_id,
                    name,
                    description,
                    destination,
                    target,
                    json,
                } => {
                    commands::net_route_create(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        route_table_id,
                        name,
                        description,
                        destination,
                        target,
                        json,
                    )
                    .await
                }
                NetRouteCommand::Get {
                    tenant_id,
                    project_id,
                    vpc_id,
                    route_table_id,
                    route_id,
                    json,
                } => {
                    commands::net_route_get(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        route_table_id,
                        route_id,
                        json,
                    )
                    .await
                }
                NetRouteCommand::Delete {
                    tenant_id,
                    project_id,
                    vpc_id,
                    route_table_id,
                    route_id,
                } => {
                    commands::net_route_delete(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        route_table_id,
                        route_id,
                    )
                    .await
                }
            },
            NetCommand::NatGw { command } => match command {
                NetNatGwCommand::List {
                    tenant_id,
                    project_id,
                    vpc_id,
                    json,
                } => {
                    commands::net_nat_gw_list(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        json,
                    )
                    .await
                }
                NetNatGwCommand::Create {
                    tenant_id,
                    project_id,
                    vpc_id,
                    name,
                    description,
                    family,
                    json,
                } => {
                    commands::net_nat_gw_create(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        name,
                        description,
                        family,
                        json,
                    )
                    .await
                }
                NetNatGwCommand::Get {
                    tenant_id,
                    project_id,
                    vpc_id,
                    nat_gateway_id,
                    json,
                } => {
                    commands::net_nat_gw_get(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        nat_gateway_id,
                        json,
                    )
                    .await
                }
                NetNatGwCommand::Delete {
                    tenant_id,
                    project_id,
                    vpc_id,
                    nat_gateway_id,
                } => {
                    commands::net_nat_gw_delete(
                        cli.endpoint,
                        cli.api_key,
                        tenant_id,
                        project_id,
                        vpc_id,
                        nat_gateway_id,
                    )
                    .await
                }
            },
            NetCommand::Dhcp { command } => match command {
                NetDhcpCommand::Pool { command } => match command {
                    NetDhcpPoolCommand::Show {
                        tenant_id,
                        project_id,
                        vpc_id,
                        json,
                    } => {
                        commands::net_dhcp_pool_show(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            json,
                        )
                        .await
                    }
                    NetDhcpPoolCommand::Set {
                        tenant_id,
                        project_id,
                        vpc_id,
                        lease_seconds,
                        exclude,
                        option,
                        json,
                    } => {
                        commands::net_dhcp_pool_set(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            lease_seconds,
                            exclude,
                            option,
                            json,
                        )
                        .await
                    }
                    NetDhcpPoolCommand::Clear {
                        tenant_id,
                        project_id,
                        vpc_id,
                    } => {
                        commands::net_dhcp_pool_clear(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                        )
                        .await
                    }
                },
                NetDhcpCommand::Reservation { command } => match command {
                    NetDhcpReservationCommand::List {
                        tenant_id,
                        project_id,
                        vpc_id,
                        json,
                    } => {
                        commands::net_dhcp_reservation_list(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            json,
                        )
                        .await
                    }
                    NetDhcpReservationCommand::Add {
                        tenant_id,
                        project_id,
                        vpc_id,
                        mac,
                        ip,
                        hostname,
                        option,
                        json,
                    } => {
                        commands::net_dhcp_reservation_add(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            mac,
                            ip,
                            hostname,
                            option,
                            json,
                        )
                        .await
                    }
                    NetDhcpReservationCommand::Get {
                        tenant_id,
                        project_id,
                        vpc_id,
                        mac,
                        json,
                    } => {
                        commands::net_dhcp_reservation_get(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            mac,
                            json,
                        )
                        .await
                    }
                    NetDhcpReservationCommand::Remove {
                        tenant_id,
                        project_id,
                        vpc_id,
                        mac,
                    } => {
                        commands::net_dhcp_reservation_remove(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            mac,
                        )
                        .await
                    }
                },
                NetDhcpCommand::Lease { command } => match command {
                    NetDhcpLeaseCommand::List {
                        tenant_id,
                        project_id,
                        vpc_id,
                        json,
                    } => {
                        commands::net_dhcp_lease_list(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            json,
                        )
                        .await
                    }
                    NetDhcpLeaseCommand::Get {
                        tenant_id,
                        project_id,
                        vpc_id,
                        mac,
                        json,
                    } => {
                        commands::net_dhcp_lease_get(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            mac,
                            json,
                        )
                        .await
                    }
                    NetDhcpLeaseCommand::Release {
                        tenant_id,
                        project_id,
                        vpc_id,
                        mac,
                    } => {
                        commands::net_dhcp_lease_release(
                            cli.endpoint,
                            cli.api_key,
                            tenant_id,
                            project_id,
                            vpc_id,
                            mac,
                        )
                        .await
                    }
                },
            },
        },
        Commands::Image { command } => match command {
            PublicImageCommand::List { json } => {
                commands::public_image_list(cli.endpoint, cli.api_key, json).await
            }
            PublicImageCommand::Add {
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
                commands::public_image_add(
                    cli.endpoint,
                    cli.api_key,
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
            PublicImageCommand::Get { image_id, json } => {
                // Re-uses the silo_image_get helper which already
                // calls the scope-agnostic /v2/images/{id} endpoint.
                commands::silo_image_get(cli.endpoint, cli.api_key, Uuid::nil(), image_id, json)
                    .await
            }
            PublicImageCommand::Delete { image_id } => {
                commands::silo_image_delete(cli.endpoint, cli.api_key, Uuid::nil(), image_id).await
            }
        },
        Commands::SshKey { command } => match command {
            PublicSshKeyCommand::List { json } => {
                commands::public_ssh_key_list(cli.endpoint, cli.api_key, json).await
            }
            PublicSshKeyCommand::Add {
                name,
                description,
                public_key,
                public_key_file,
                json,
            } => {
                commands::public_ssh_key_add(
                    cli.endpoint,
                    cli.api_key,
                    name,
                    description,
                    public_key,
                    public_key_file,
                    json,
                )
                .await
            }
            PublicSshKeyCommand::Show { key_id, json } => {
                commands::ssh_key_show(cli.endpoint, cli.api_key, key_id, json).await
            }
            PublicSshKeyCommand::Delete { key_id } => {
                commands::ssh_key_delete(cli.endpoint, cli.api_key, key_id).await
            }
        },
        Commands::Auth { command } => match command {
            AuthCommand::Image { command } => match command {
                AuthImageCommand::List { json } => {
                    commands::auth_image_list(cli.endpoint, cli.api_key, json).await
                }
                AuthImageCommand::Add {
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
                    commands::auth_image_add(
                        cli.endpoint,
                        cli.api_key,
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
            },
            AuthCommand::SshKey { command } => match command {
                AuthSshKeyCommand::List { json } => {
                    commands::auth_ssh_key_list(cli.endpoint, cli.api_key, json).await
                }
                AuthSshKeyCommand::Add {
                    name,
                    description,
                    public_key,
                    public_key_file,
                    json,
                } => {
                    commands::auth_ssh_key_add(
                        cli.endpoint,
                        cli.api_key,
                        name,
                        description,
                        public_key,
                        public_key_file,
                        json,
                    )
                    .await
                }
            },
        },
        Commands::Meta { command } => match command {
            MetaCommand::List { scope, id, json } => {
                commands::meta_list(cli.endpoint, cli.api_key, scope.into(), id, json).await
            }
            MetaCommand::Get {
                scope,
                id,
                key,
                json,
            } => commands::meta_get(cli.endpoint, cli.api_key, scope.into(), id, key, json).await,
            MetaCommand::Set {
                scope,
                id,
                key,
                value,
                guest_visible,
                guest_writable,
                json,
            } => {
                commands::meta_set(
                    cli.endpoint,
                    cli.api_key,
                    scope.into(),
                    id,
                    key,
                    value,
                    guest_visible,
                    guest_writable,
                    json,
                )
                .await
            }
            MetaCommand::Unset { scope, id, key } => {
                commands::meta_unset(cli.endpoint, cli.api_key, scope.into(), id, key).await
            }
            MetaCommand::Realized { instance, json } => {
                commands::meta_realized(cli.endpoint, cli.api_key, instance, json).await
            }
        },
        Commands::Storage { command } => match command {
            StorageCommand::Cluster { command } => match command {
                StorageClusterCommand::List { json } => {
                    commands::storage_cluster_list(cli.endpoint, cli.api_key, json).await
                }
                StorageClusterCommand::Show { ident, json } => {
                    commands::storage_cluster_show(cli.endpoint, cli.api_key, ident, json).await
                }
                StorageClusterCommand::Add {
                    name,
                    cluster_endpoint,
                    admin_token,
                    admin_token_stdin,
                    surface,
                    default_region,
                    display_name,
                    json,
                } => {
                    commands::storage_cluster_add(
                        cli.endpoint,
                        cli.api_key,
                        name,
                        cluster_endpoint,
                        admin_token,
                        admin_token_stdin,
                        surface.into(),
                        default_region,
                        display_name,
                        json,
                    )
                    .await
                }
                StorageClusterCommand::Delete { cluster_id } => {
                    commands::storage_cluster_delete(cli.endpoint, cli.api_key, cluster_id).await
                }
                StorageClusterCommand::Health { ident, json } => {
                    commands::storage_cluster_health(cli.endpoint, cli.api_key, ident, json).await
                }
                StorageClusterCommand::SetPresigner {
                    ident,
                    s3_endpoint,
                    access_key_id,
                    secret_access_key,
                    secret_access_key_stdin,
                    json,
                } => {
                    commands::storage_cluster_set_presigner(
                        cli.endpoint,
                        cli.api_key,
                        ident,
                        s3_endpoint,
                        access_key_id,
                        secret_access_key,
                        secret_access_key_stdin,
                        json,
                    )
                    .await
                }
                StorageClusterCommand::ClearPresigner { ident } => {
                    commands::storage_cluster_clear_presigner(cli.endpoint, cli.api_key, ident)
                        .await
                }
            },
        },
        Commands::Config { command } => match command {
            ConfigCommand::List { json } => {
                commands::config_list(cli.endpoint, cli.api_key, json).await
            }
            ConfigCommand::Get { key, json } => {
                commands::config_get(cli.endpoint, cli.api_key, key, json).await
            }
            ConfigCommand::Set { key, value } => {
                commands::config_set(cli.endpoint, cli.api_key, key, value).await
            }
            ConfigCommand::Reset { key } => {
                commands::config_reset(cli.endpoint, cli.api_key, key).await
            }
        },
        Commands::Install {
            name,
            stamp,
            channel_url,
            list,
        } => {
            // install is sync (blocking reqwest + child processes).
            // Run on a blocking-task slot.
            tokio::task::spawn_blocking(move || {
                install::run(install::InstallOpts {
                    name,
                    stamp,
                    channel_url,
                    list,
                })
            })
            .await
            .map_err(|e| anyhow::anyhow!("install task panicked: {e}"))?
        }
        Commands::SelfUpdate {
            channel_url,
            install_dir,
            check,
        } => {
            // self-update is sync (blocking reqwest + filesystem). Run
            // it on a blocking-task slot to avoid wedging the tokio
            // runtime; the call returns a Result that we propagate.
            tokio::task::spawn_blocking(move || {
                self_update::run(self_update::SelfUpdateOpts {
                    channel_url,
                    install_dir,
                    check,
                })
            })
            .await
            .map_err(|e| anyhow::anyhow!("self-update task panicked: {e}"))?
        }
    }
}

impl From<StorageSurfaceArg> for tritond_client::types::StorageClusterSurface {
    fn from(s: StorageSurfaceArg) -> Self {
        match s {
            StorageSurfaceArg::S3 => Self::S3,
            StorageSurfaceArg::Fs => Self::Fs,
            StorageSurfaceArg::Block => Self::Block,
        }
    }
}
