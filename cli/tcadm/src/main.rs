// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tcadm — Triton Cloud operator CLI.

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
    Instance {
        #[command(subcommand)]
        command: InstanceCommand,
    },
    Disk {
        #[command(subcommand)]
        command: DiskCommand,
    },
    Nic {
        #[command(subcommand)]
        command: NicCommand,
    },
    Vpc {
        #[command(subcommand)]
        command: VpcCommand,
    },
    Subnet {
        #[command(subcommand)]
        command: SubnetCommand,
    },
    FloatingIp {
        #[command(subcommand)]
        command: FloatingIpCommand,
    },
    FirewallRule {
        #[command(subcommand)]
        command: FirewallRuleCommand,
    },
    NatGateway {
        #[command(subcommand)]
        command: NatGatewayCommand,
    },
    RouteTable {
        #[command(subcommand)]
        command: RouteTableCommand,
    },
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
    ImageV1 {
        #[command(subcommand)]
        command: ImageV1Command,
    },
    SshKeyV1 {
        #[command(subcommand)]
        command: SshKeyV1Command,
    },
    /// Fleet-admin operator commands. Capability-gated; callers
    /// without the right capability see 404 NotFound.
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },
    /// Client-side composition over the typed /v1/system/* list
    /// endpoints. Argument is parsed as UUID, then IP, then MAC,
    /// then (with --kind) name. Capability: SystemRead.
    Find {
        /// Freeform input: a UUID, an IP address, or a name. If
        /// ambiguous (e.g. a hex string that could be uuid or name),
        /// UUID and IP are tried first.
        what: String,
        /// Disambiguate when the freeform input is a name or when
        /// you want to limit the search to one kind. Accepted:
        /// instance, image, cn, ip, nic.
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        json: bool,
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
    /// Retrofit a storage workspace binding onto an existing tenant.
    ///
    /// For tenants created before `storage.default_s3_cluster_id`
    /// was registered. Idempotent on the mantad side (keyed by
    /// tenant_uuid), so safe to retry. Refuses to rebind a tenant
    /// that already has a binding (409); use the cluster-level
    /// admin tooling if you need to swap clusters.
    InitStorage {
        silo_id: Uuid,
        tenant_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Mint a tenant-bound operator user.
    ///
    /// Lands a User with `tenant_id` set to the tenant on the URL,
    /// `is_root: false`, empty capability set, and a bcrypt-hashed
    /// password. Use for test / non-federated tenant principals
    /// when no external IdP is available (or for verification
    /// tooling).
    CreateUser {
        silo_id: Uuid,
        tenant_id: Uuid,
        #[arg(long)]
        username: String,
        #[arg(long)]
        password: String,
        #[arg(long)]
        json: bool,
    },
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
    /// Manage the project's resource quota.
    Quota {
        #[command(subcommand)]
        command: TenantProjectQuotaCommand,
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
    /// Operator-initiated unwind: inject an error at every pending
    /// saga node so the catalog's own undos run. The currently-running
    /// action (if any) completes its natural outcome first.
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

#[derive(Subcommand)]
enum InstanceCommand {
    /// List instances. One of --image, --cn, or --tenant+--project
    /// is required; --state narrows client-side.
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
    /// Create a new instance under a project.
    #[command(name = "create")]
    Create {
        #[arg(long)]
        tenant: Uuid,
        #[arg(long)]
        project: Uuid,
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
    /// Delete an instance (must be Stopped or Failed).
    Delete {
        instance_id: Uuid,
        /// Force-delete a non-terminal instance; server still enforces
        /// the Stopped/Failed gate without this flag.
        #[arg(long)]
        force: bool,
    },
    Start {
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    Stop {
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    Restart {
        instance_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DiskCommand {
    /// List disks. Requires --instance.
    List {
        #[arg(long)]
        instance: Uuid,
        #[arg(long)]
        json: bool,
    },
    Show {
        disk_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum NicCommand {
    /// List NICs. Requires one of --ip, --subnet, or --instance.
    List {
        #[arg(long)]
        ip: Option<std::net::IpAddr>,
        #[arg(long)]
        subnet: Option<Uuid>,
        #[arg(long)]
        instance: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    Show {
        nic_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum VpcCommand {
    /// List VPCs in a project.
    List {
        #[arg(long)]
        tenant: Uuid,
        #[arg(long)]
        project: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read a single VPC by UUID.
    Show {
        vpc_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a new VPC under a project.
    Create {
        #[arg(long)]
        tenant: Uuid,
        #[arg(long)]
        project: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// IPv4 CIDR block (one of ipv4-block / ipv6-block required).
        #[arg(long = "ipv4-block")]
        ipv4_block: Option<String>,
        /// IPv6 CIDR block.
        #[arg(long = "ipv6-block")]
        ipv6_block: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a VPC. The server enforces the dependency gate
    /// (subnets, firewall rules, NAT gateways, route tables must
    /// be empty); 409 Conflict if anything still references it.
    Delete { vpc_id: Uuid },
}

#[derive(Subcommand)]
enum SubnetCommand {
    /// List subnets in a VPC.
    List {
        #[arg(long)]
        vpc: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read a single subnet by UUID.
    Show {
        subnet_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a subnet inside a VPC.
    Create {
        #[arg(long)]
        vpc: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// IPv4 CIDR block (one of ipv4-block / ipv6-block required).
        #[arg(long = "ipv4-block")]
        ipv4_block: Option<String>,
        #[arg(long = "ipv6-block")]
        ipv6_block: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a subnet. The server enforces the dependency gate
    /// (no NICs allocated from this subnet); 409 Conflict otherwise.
    Delete { subnet_id: Uuid },
}

#[derive(Subcommand)]
enum ImageV1Command {
    /// List images at a given scope. Only `--scope=public` works today.
    List {
        #[arg(long, default_value = "public")]
        scope: String,
        #[arg(long)]
        json: bool,
    },
    Show {
        image_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SshKeyV1Command {
    /// List SSH keys at a given scope. Only `--scope=public` works today.
    List {
        #[arg(long, default_value = "public")]
        scope: String,
        #[arg(long)]
        json: bool,
    },
    /// Read a single SSH key by UUID.
    Show {
        key_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum FloatingIpCommand {
    /// List floating IPs in a project.
    List {
        #[arg(long)]
        tenant: Uuid,
        #[arg(long)]
        project: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Read a single floating IP by UUID.
    Show {
        floating_ip_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Allocate a new floating IP into a project.
    Create {
        #[arg(long)]
        tenant: Uuid,
        #[arg(long)]
        project: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// Address family to allocate from: ipv4 or ipv6.
        #[arg(long, default_value = "ipv4")]
        family: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a floating IP. The IP must already be detached;
    /// server returns 409 Conflict if anything is still attached.
    Delete { floating_ip_id: Uuid },
    /// Attach a floating IP to a NIC.
    Attach {
        floating_ip_id: Uuid,
        #[arg(long)]
        nic: Uuid,
    },
    /// Detach a floating IP from its current NIC.
    Detach { floating_ip_id: Uuid },
}

#[derive(Subcommand)]
enum FirewallRuleCommand {
    /// List firewall rules. Provide one of --vpc, --project, --tenant.
    List {
        #[arg(long)]
        vpc: Option<Uuid>,
        #[arg(long)]
        project: Option<Uuid>,
        #[arg(long)]
        tenant: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single firewall rule by UUID.
    Show {
        firewall_rule_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a firewall rule on a VPC.
    Create {
        #[arg(long)]
        vpc: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// allow | deny
        #[arg(long)]
        action: String,
        /// inbound | outbound
        #[arg(long)]
        direction: String,
        /// any | tcp | udp | icmp4 | icmp6
        #[arg(long, default_value = "any")]
        protocol: String,
        #[arg(long)]
        priority: u16,
        /// Source CIDR (optional; omitted means any).
        #[arg(long = "source-cidr")]
        source_cidr: Option<String>,
        /// Destination CIDR (optional; omitted means any).
        #[arg(long = "destination-cidr")]
        destination_cidr: Option<String>,
        /// Source ports as `low-high` (TCP/UDP only).
        #[arg(long = "source-ports")]
        source_ports: Option<String>,
        /// Destination ports as `low-high` (TCP/UDP only).
        #[arg(long = "destination-ports")]
        destination_ports: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a firewall rule by UUID.
    Delete { firewall_rule_id: Uuid },
}

#[derive(Subcommand)]
enum NatGatewayCommand {
    /// List NAT gateways. Provide one of --vpc, --project, --tenant.
    List {
        #[arg(long)]
        vpc: Option<Uuid>,
        #[arg(long)]
        project: Option<Uuid>,
        #[arg(long)]
        tenant: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single NAT gateway by UUID.
    Show {
        nat_gateway_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a NAT gateway on a VPC.
    Create {
        #[arg(long)]
        vpc: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// ipv4 | ipv6
        #[arg(long, default_value = "ipv4")]
        family: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a NAT gateway. Server enforces that no route still
    /// targets it; 409 Conflict otherwise.
    Delete { nat_gateway_id: Uuid },
}

#[derive(Subcommand)]
enum RouteTableCommand {
    /// List route tables. Provide one of --vpc, --project, --tenant.
    List {
        #[arg(long)]
        vpc: Option<Uuid>,
        #[arg(long)]
        project: Option<Uuid>,
        #[arg(long)]
        tenant: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single route table by UUID.
    Show {
        route_table_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a non-main route table inside a VPC. POSTs to
    /// `/v1/route-tables?vpc=<uuid>`.
    Create {
        #[arg(long)]
        vpc: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a route table. The VPC's main route table cannot be
    /// deleted; server returns 409 Conflict.
    Delete { route_table_id: Uuid },
}

#[derive(Subcommand)]
enum RouteCommand {
    /// List routes. Provide one of --route-table, --project, --tenant.
    List {
        #[arg(long = "route-table")]
        route_table: Option<Uuid>,
        #[arg(long)]
        project: Option<Uuid>,
        #[arg(long)]
        tenant: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    /// Read a single route by UUID.
    Show {
        route_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Create a route in a route table. Exactly one of `--target-*`
    /// flags must be provided; NAT-gateway targets must live in the
    /// same VPC as the route table.
    Create {
        #[arg(long = "route-table")]
        route_table: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// Destination CIDR.
        #[arg(long)]
        destination: String,
        /// Target: send to a NAT gateway by UUID.
        #[arg(long = "target-nat-gateway")]
        target_nat_gateway: Option<Uuid>,
        /// Target: blackhole the traffic.
        #[arg(long = "target-blackhole")]
        target_blackhole: bool,
        /// Target: ICMP-reject the traffic.
        #[arg(long = "target-reject")]
        target_reject: bool,
        /// Target: send to the VPC's virtual gateway.
        #[arg(long = "target-virtual-gateway")]
        target_virtual_gateway: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete a route by UUID. Server forbids deleting system routes
    /// (e.g. the default route in the main route table); 409 Conflict.
    Delete { route_id: Uuid },
}

/// Fleet-admin operator commands under `/v1/system/`. A caller
/// without the right capability sees the same 404 as a missing
/// resource.
#[derive(Subcommand)]
enum SystemCommand {
    /// Fleet-wide instance search ("which VMs use image X?",
    /// "what is on CN Y?"). Capability: `SystemRead`.
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
    /// "Which VMs use this image?" Single FDB range read against
    /// the `idx/image/<image>/` index.
    ImagesUsing {
        image_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// "What's on this CN?"
    CnInstances {
        cn_id: Uuid,
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
    /// Fetch per-silo utilization. Returns 501 until quota
    /// accounting lands.
    Utilization {
        #[arg(long)]
        json: bool,
    },
    /// Find a DHCP lease by MAC across every VPC. Backed by the
    /// `dhcp_lease/by_mac/<mac>` index.
    DhcpLeaseShow {
        mac: String,
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
    UserRevoke { user_id: Uuid, capability: String },
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
            InstanceCommand::Create {
                tenant,
                project,
                name,
                description,
                image_id,
                primary_subnet_id,
                ssh_key_ids,
                cpu,
                memory_bytes,
                json,
            } => {
                commands::instance_create_v1(
                    cli.endpoint,
                    cli.api_key,
                    tenant,
                    project,
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
            InstanceCommand::Delete { instance_id, force } => {
                commands::instance_delete_v1(cli.endpoint, cli.api_key, instance_id, force).await
            }
            InstanceCommand::Start { instance_id, json } => {
                commands::instance_lifecycle_v1(
                    cli.endpoint,
                    cli.api_key,
                    instance_id,
                    "start",
                    json,
                )
                .await
            }
            InstanceCommand::Stop { instance_id, json } => {
                commands::instance_lifecycle_v1(
                    cli.endpoint,
                    cli.api_key,
                    instance_id,
                    "stop",
                    json,
                )
                .await
            }
            InstanceCommand::Restart { instance_id, json } => {
                commands::instance_lifecycle_v1(
                    cli.endpoint,
                    cli.api_key,
                    instance_id,
                    "restart",
                    json,
                )
                .await
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
                commands::system_nics_v1(cli.endpoint, cli.api_key, ip, subnet, instance, json)
                    .await
            }
            SystemCommand::ImagesUsing { image_id, json } => {
                commands::system_images_using_v1(cli.endpoint, cli.api_key, image_id, json).await
            }
            SystemCommand::CnInstances { cn_id, json } => {
                commands::system_cn_instances_v1(cli.endpoint, cli.api_key, cn_id, json).await
            }
            SystemCommand::Cns { state, json } => {
                commands::system_cns_v1(cli.endpoint, cli.api_key, state.map(Into::into), json)
                    .await
            }
            SystemCommand::Utilization { json } => {
                commands::system_utilization_v1(cli.endpoint, cli.api_key, json).await
            }
            SystemCommand::DhcpLeaseShow { mac, json } => {
                commands::dhcp_lease_show_v1(cli.endpoint, cli.api_key, mac, json).await
            }
            SystemCommand::UserGrant {
                user_id,
                capability,
            } => {
                commands::system_user_grant_v1(cli.endpoint, cli.api_key, user_id, capability).await
            }
            SystemCommand::UserRevoke {
                user_id,
                capability,
            } => {
                commands::system_user_revoke_v1(cli.endpoint, cli.api_key, user_id, capability)
                    .await
            }
        },
        Commands::Disk { command } => match command {
            DiskCommand::List { instance, json } => {
                commands::disk_list_v1(cli.endpoint, cli.api_key, instance, json).await
            }
            DiskCommand::Show { disk_id, json } => {
                commands::disk_show_v1(cli.endpoint, cli.api_key, disk_id, json).await
            }
        },
        Commands::Nic { command } => match command {
            NicCommand::List {
                ip,
                subnet,
                instance,
                json,
            } => commands::nic_list_v1(cli.endpoint, cli.api_key, ip, subnet, instance, json).await,
            NicCommand::Show { nic_id, json } => {
                commands::nic_show_v1(cli.endpoint, cli.api_key, nic_id, json).await
            }
        },
        Commands::Vpc { command } => match command {
            VpcCommand::List {
                tenant,
                project,
                json,
            } => commands::vpc_list_v1(cli.endpoint, cli.api_key, tenant, project, json).await,
            VpcCommand::Show { vpc_id, json } => {
                commands::vpc_show_v1(cli.endpoint, cli.api_key, vpc_id, json).await
            }
            VpcCommand::Create {
                tenant,
                project,
                name,
                description,
                ipv4_block,
                ipv6_block,
                json,
            } => {
                commands::vpc_create_v1(
                    cli.endpoint,
                    cli.api_key,
                    tenant,
                    project,
                    name,
                    description,
                    ipv4_block,
                    ipv6_block,
                    json,
                )
                .await
            }
            VpcCommand::Delete { vpc_id } => {
                commands::vpc_delete_v1(cli.endpoint, cli.api_key, vpc_id).await
            }
        },
        Commands::Subnet { command } => match command {
            SubnetCommand::List { vpc, json } => {
                commands::subnet_list_v1(cli.endpoint, cli.api_key, vpc, json).await
            }
            SubnetCommand::Show { subnet_id, json } => {
                commands::subnet_show_v1(cli.endpoint, cli.api_key, subnet_id, json).await
            }
            SubnetCommand::Create {
                vpc,
                name,
                description,
                ipv4_block,
                ipv6_block,
                json,
            } => {
                commands::subnet_create_v1(
                    cli.endpoint,
                    cli.api_key,
                    vpc,
                    name,
                    description,
                    ipv4_block,
                    ipv6_block,
                    json,
                )
                .await
            }
            SubnetCommand::Delete { subnet_id } => {
                commands::subnet_delete_v1(cli.endpoint, cli.api_key, subnet_id).await
            }
        },
        Commands::ImageV1 { command } => match command {
            ImageV1Command::List { scope, json } => {
                commands::image_list_v1(cli.endpoint, cli.api_key, scope, json).await
            }
            ImageV1Command::Show { image_id, json } => {
                commands::image_show_v1(cli.endpoint, cli.api_key, image_id, json).await
            }
        },
        Commands::SshKeyV1 { command } => match command {
            SshKeyV1Command::List { scope, json } => {
                commands::ssh_key_list_v1(cli.endpoint, cli.api_key, scope, json).await
            }
            SshKeyV1Command::Show { key_id, json } => {
                commands::ssh_key_show_v1(cli.endpoint, cli.api_key, key_id, json).await
            }
        },
        Commands::FloatingIp { command } => match command {
            FloatingIpCommand::List {
                tenant,
                project,
                json,
            } => {
                commands::floating_ip_list_v1(cli.endpoint, cli.api_key, tenant, project, json)
                    .await
            }
            FloatingIpCommand::Show {
                floating_ip_id,
                json,
            } => {
                commands::floating_ip_show_v1(cli.endpoint, cli.api_key, floating_ip_id, json).await
            }
            FloatingIpCommand::Create {
                tenant,
                project,
                name,
                description,
                family,
                json,
            } => {
                commands::floating_ip_create_v1(
                    cli.endpoint,
                    cli.api_key,
                    tenant,
                    project,
                    name,
                    description,
                    family,
                    json,
                )
                .await
            }
            FloatingIpCommand::Delete { floating_ip_id } => {
                commands::floating_ip_delete_v1(cli.endpoint, cli.api_key, floating_ip_id).await
            }
            FloatingIpCommand::Attach {
                floating_ip_id,
                nic,
            } => {
                commands::floating_ip_attach_v1(cli.endpoint, cli.api_key, floating_ip_id, nic)
                    .await
            }
            FloatingIpCommand::Detach { floating_ip_id } => {
                commands::floating_ip_detach_v1(cli.endpoint, cli.api_key, floating_ip_id).await
            }
        },
        Commands::FirewallRule { command } => match command {
            FirewallRuleCommand::List {
                vpc,
                project,
                tenant,
                json,
            } => {
                commands::firewall_rule_list_v1(
                    cli.endpoint,
                    cli.api_key,
                    vpc,
                    project,
                    tenant,
                    json,
                )
                .await
            }
            FirewallRuleCommand::Show {
                firewall_rule_id,
                json,
            } => {
                commands::firewall_rule_show_v1(cli.endpoint, cli.api_key, firewall_rule_id, json)
                    .await
            }
            FirewallRuleCommand::Create {
                vpc,
                name,
                description,
                action,
                direction,
                protocol,
                priority,
                source_cidr,
                destination_cidr,
                source_ports,
                destination_ports,
                json,
            } => {
                commands::firewall_rule_create_v1(
                    cli.endpoint,
                    cli.api_key,
                    vpc,
                    name,
                    description,
                    action,
                    direction,
                    protocol,
                    priority,
                    source_cidr,
                    destination_cidr,
                    source_ports,
                    destination_ports,
                    json,
                )
                .await
            }
            FirewallRuleCommand::Delete { firewall_rule_id } => {
                commands::firewall_rule_delete_v1(cli.endpoint, cli.api_key, firewall_rule_id).await
            }
        },
        Commands::NatGateway { command } => match command {
            NatGatewayCommand::List {
                vpc,
                project,
                tenant,
                json,
            } => {
                commands::nat_gateway_list_v1(cli.endpoint, cli.api_key, vpc, project, tenant, json)
                    .await
            }
            NatGatewayCommand::Show {
                nat_gateway_id,
                json,
            } => {
                commands::nat_gateway_show_v1(cli.endpoint, cli.api_key, nat_gateway_id, json).await
            }
            NatGatewayCommand::Create {
                vpc,
                name,
                description,
                family,
                json,
            } => {
                commands::nat_gateway_create_v1(
                    cli.endpoint,
                    cli.api_key,
                    vpc,
                    name,
                    description,
                    family,
                    json,
                )
                .await
            }
            NatGatewayCommand::Delete { nat_gateway_id } => {
                commands::nat_gateway_delete_v1(cli.endpoint, cli.api_key, nat_gateway_id).await
            }
        },
        Commands::RouteTable { command } => match command {
            RouteTableCommand::List {
                vpc,
                project,
                tenant,
                json,
            } => {
                commands::route_table_list_v1(cli.endpoint, cli.api_key, vpc, project, tenant, json)
                    .await
            }
            RouteTableCommand::Show {
                route_table_id,
                json,
            } => {
                commands::route_table_show_v1(cli.endpoint, cli.api_key, route_table_id, json).await
            }
            RouteTableCommand::Create {
                vpc,
                name,
                description,
                json,
            } => {
                commands::route_table_create_v1(
                    cli.endpoint,
                    cli.api_key,
                    vpc,
                    name,
                    description,
                    json,
                )
                .await
            }
            RouteTableCommand::Delete { route_table_id } => {
                commands::route_table_delete_v1(cli.endpoint, cli.api_key, route_table_id).await
            }
        },
        Commands::Route { command } => match command {
            RouteCommand::List {
                route_table,
                project,
                tenant,
                json,
            } => {
                commands::route_list_v1(
                    cli.endpoint,
                    cli.api_key,
                    route_table,
                    project,
                    tenant,
                    json,
                )
                .await
            }
            RouteCommand::Show { route_id, json } => {
                commands::route_show_v1(cli.endpoint, cli.api_key, route_id, json).await
            }
            RouteCommand::Create {
                route_table,
                name,
                description,
                destination,
                target_nat_gateway,
                target_blackhole,
                target_reject,
                target_virtual_gateway,
                json,
            } => {
                commands::route_create_v1(
                    cli.endpoint,
                    cli.api_key,
                    route_table,
                    name,
                    description,
                    destination,
                    target_nat_gateway,
                    target_blackhole,
                    target_reject,
                    target_virtual_gateway,
                    json,
                )
                .await
            }
            RouteCommand::Delete { route_id } => {
                commands::route_delete_v1(cli.endpoint, cli.api_key, route_id).await
            }
        },
        Commands::Find { what, kind, json } => {
            commands::find_v1(cli.endpoint, cli.api_key, what, kind, json).await
        }
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
            TenantCommand::InitStorage {
                silo_id,
                tenant_id,
                json,
            } => {
                commands::tenant_init_storage(cli.endpoint, cli.api_key, silo_id, tenant_id, json)
                    .await
            }
            TenantCommand::CreateUser {
                silo_id,
                tenant_id,
                username,
                password,
                json,
            } => {
                commands::tenant_create_user(
                    cli.endpoint,
                    cli.api_key,
                    silo_id,
                    tenant_id,
                    username,
                    password,
                    json,
                )
                .await
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
                // calls the scope-agnostic /v1/images/{id} endpoint.
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
