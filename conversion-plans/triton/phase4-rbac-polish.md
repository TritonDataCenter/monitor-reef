<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Phase 4: RBAC and Polish

## Goal

Implement RBAC (Role-Based Access Control) commands, top-level shortcuts, and shell completions.

## Prerequisites

- Phase 0-3 complete

## Tasks

### Task 1: Implement RBAC Commands (`commands/rbac.rs`)

Reference: `target/node-triton/lib/do_rbac/`

```rust
//! RBAC (Role-Based Access Control) management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum RbacCommand {
    /// Manage RBAC users
    User {
        #[command(subcommand)]
        command: RbacUserCommand,
    },
    /// Manage RBAC roles
    Role {
        #[command(subcommand)]
        command: RbacRoleCommand,
    },
    /// Manage RBAC policies
    Policy {
        #[command(subcommand)]
        command: RbacPolicyCommand,
    },
    /// Manage role tags on resources
    RoleTag {
        #[command(subcommand)]
        command: RbacRoleTagCommand,
    },
}

// =============================================================================
// User Commands
// =============================================================================

#[derive(Subcommand)]
pub enum RbacUserCommand {
    /// List RBAC users
    #[command(alias = "ls")]
    List,
    /// Get user details
    Get(UserGetArgs),
    /// Create user
    Create(UserCreateArgs),
    /// Update user
    Update(UserUpdateArgs),
    /// Delete user(s)
    #[command(alias = "rm")]
    Delete(UserDeleteArgs),
}

#[derive(Args)]
pub struct UserGetArgs {
    /// User login or UUID
    user: String,
}

#[derive(Args)]
pub struct UserCreateArgs {
    /// User login
    login: String,
    /// Email address
    #[arg(long)]
    email: String,
    /// Password (will prompt if not provided)
    #[arg(long)]
    password: Option<String>,
}

#[derive(Args)]
pub struct UserUpdateArgs {
    /// User login or UUID
    user: String,
    /// New email
    #[arg(long)]
    email: Option<String>,
    /// New company name
    #[arg(long)]
    company_name: Option<String>,
}

#[derive(Args)]
pub struct UserDeleteArgs {
    /// User login(s) or UUID(s)
    users: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
}

// =============================================================================
// Role Commands
// =============================================================================

#[derive(Subcommand)]
pub enum RbacRoleCommand {
    /// List RBAC roles
    #[command(alias = "ls")]
    List,
    /// Get role details
    Get(RoleGetArgs),
    /// Create role
    Create(RoleCreateArgs),
    /// Update role
    Update(RoleUpdateArgs),
    /// Delete role(s)
    #[command(alias = "rm")]
    Delete(RoleDeleteArgs),
}

#[derive(Args)]
pub struct RoleGetArgs {
    /// Role name or UUID
    role: String,
}

#[derive(Args)]
pub struct RoleCreateArgs {
    /// Role name
    name: String,
    /// Policies to attach (comma-separated or multiple flags)
    #[arg(long)]
    policy: Option<Vec<String>>,
    /// Members (user logins, comma-separated or multiple flags)
    #[arg(long)]
    member: Option<Vec<String>>,
    /// Default members (user logins)
    #[arg(long)]
    default_member: Option<Vec<String>>,
}

#[derive(Args)]
pub struct RoleUpdateArgs {
    /// Role name or UUID
    role: String,
    /// New name
    #[arg(long)]
    name: Option<String>,
    /// Policies to attach
    #[arg(long)]
    policy: Option<Vec<String>>,
    /// Members to add
    #[arg(long)]
    add_member: Option<Vec<String>>,
    /// Members to remove
    #[arg(long)]
    remove_member: Option<Vec<String>>,
}

#[derive(Args)]
pub struct RoleDeleteArgs {
    /// Role name(s) or UUID(s)
    roles: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
}

// =============================================================================
// Policy Commands
// =============================================================================

#[derive(Subcommand)]
pub enum RbacPolicyCommand {
    /// List RBAC policies
    #[command(alias = "ls")]
    List,
    /// Get policy details
    Get(PolicyGetArgs),
    /// Create policy
    Create(PolicyCreateArgs),
    /// Update policy
    Update(PolicyUpdateArgs),
    /// Delete policy(s)
    #[command(alias = "rm")]
    Delete(PolicyDeleteArgs),
}

#[derive(Args)]
pub struct PolicyGetArgs {
    /// Policy name or UUID
    policy: String,
}

#[derive(Args)]
pub struct PolicyCreateArgs {
    /// Policy name
    name: String,
    /// Policy rules (JSON file or inline)
    #[arg(long)]
    rules: String,
    /// Description
    #[arg(long)]
    description: Option<String>,
}

#[derive(Args)]
pub struct PolicyUpdateArgs {
    /// Policy name or UUID
    policy: String,
    /// New name
    #[arg(long)]
    name: Option<String>,
    /// New rules
    #[arg(long)]
    rules: Option<String>,
    /// New description
    #[arg(long)]
    description: Option<String>,
}

#[derive(Args)]
pub struct PolicyDeleteArgs {
    /// Policy name(s) or UUID(s)
    policies: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
}

// =============================================================================
// Role Tag Commands
// =============================================================================

#[derive(Subcommand)]
pub enum RbacRoleTagCommand {
    /// Set role tags on a resource
    Set(RoleTagSetArgs),
    /// Get role tags on a resource
    Get(RoleTagGetArgs),
    /// Remove role tags from a resource
    Remove(RoleTagRemoveArgs),
}

#[derive(Args)]
pub struct RoleTagSetArgs {
    /// Resource type (machine, image, network, etc.)
    resource_type: String,
    /// Resource ID or name
    resource: String,
    /// Role tag(s) to set (role=tag format)
    tags: Vec<String>,
}

#[derive(Args)]
pub struct RoleTagGetArgs {
    /// Resource type
    resource_type: String,
    /// Resource ID or name
    resource: String,
}

#[derive(Args)]
pub struct RoleTagRemoveArgs {
    /// Resource type
    resource_type: String,
    /// Resource ID or name
    resource: String,
    /// Role tag(s) to remove
    tags: Vec<String>,
}

// =============================================================================
// Implementation
// =============================================================================

impl RbacCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::User { command } => command.run(client, use_json).await,
            Self::Role { command } => command.run(client, use_json).await,
            Self::Policy { command } => command.run(client, use_json).await,
            Self::RoleTag { command } => command.run(client, use_json).await,
        }
    }
}

impl RbacUserCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_users(client, use_json).await,
            Self::Get(args) => get_user(args, client, use_json).await,
            Self::Create(args) => create_user(args, client, use_json).await,
            Self::Update(args) => update_user(args, client, use_json).await,
            Self::Delete(args) => delete_users(args, client).await,
        }
    }
}

impl RbacRoleCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_roles(client, use_json).await,
            Self::Get(args) => get_role(args, client, use_json).await,
            Self::Create(args) => create_role(args, client, use_json).await,
            Self::Update(args) => update_role(args, client, use_json).await,
            Self::Delete(args) => delete_roles(args, client).await,
        }
    }
}

impl RbacPolicyCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_policies(client, use_json).await,
            Self::Get(args) => get_policy(args, client, use_json).await,
            Self::Create(args) => create_policy(args, client, use_json).await,
            Self::Update(args) => update_policy(args, client, use_json).await,
            Self::Delete(args) => delete_policies(args, client).await,
        }
    }
}

impl RbacRoleTagCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Set(args) => set_role_tags(args, client).await,
            Self::Get(args) => get_role_tags(args, client, use_json).await,
            Self::Remove(args) => remove_role_tags(args, client).await,
        }
    }
}

// =============================================================================
// User Implementation
// =============================================================================

async fn list_users(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_users()
        .account(account)
        .send()
        .await?;

    let users = response.into_inner();

    if use_json {
        json::print_json(&users)?;
    } else {
        let mut tbl = table::create_table(&["UUID", "LOGIN", "EMAIL"]);
        for user in &users {
            tbl.add_row(vec![
                &user.id.to_string()[..8],
                &user.login,
                &user.email,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_user(args: UserGetArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let user_id = resolve_user(&args.user, client).await?;

    let response = client.inner().inner()
        .get_user()
        .account(account)
        .uuid(&user_id)
        .send()
        .await?;

    let user = response.into_inner();

    if use_json {
        json::print_json(&user)?;
    } else {
        println!("UUID:  {}", user.id);
        println!("Login: {}", user.login);
        println!("Email: {}", user.email);
    }

    Ok(())
}

async fn create_user(args: UserCreateArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;

    // Prompt for password if not provided
    let password = match args.password {
        Some(p) => p,
        None => {
            use dialoguer::Password;
            Password::new()
                .with_prompt("Password")
                .with_confirmation("Confirm password", "Passwords do not match")
                .interact()?
        }
    };

    let request = cloudapi_client::CreateUserRequest {
        login: args.login,
        email: args.email,
        password,
        ..Default::default()
    };

    let response = client.inner().inner()
        .create_user()
        .account(account)
        .body(request)
        .send()
        .await?;

    let user = response.into_inner();
    println!("Created user '{}' ({})", user.login, user.id);

    if use_json {
        json::print_json(&user)?;
    }

    Ok(())
}

async fn update_user(args: UserUpdateArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let user_id = resolve_user(&args.user, client).await?;

    let request = cloudapi_client::UpdateUserRequest {
        email: args.email,
        company_name: args.company_name,
        ..Default::default()
    };

    let response = client.inner().inner()
        .update_user()
        .account(account)
        .uuid(&user_id)
        .body(request)
        .send()
        .await?;

    let user = response.into_inner();
    println!("Updated user '{}'", user.login);

    if use_json {
        json::print_json(&user)?;
    }

    Ok(())
}

async fn delete_users(args: UserDeleteArgs, client: &AuthenticatedClient) -> Result<()> {
    for user_ref in &args.users {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete user '{}'?", user_ref))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        let user_id = resolve_user(user_ref, client).await?;
        let account = &client.auth_state().account;

        client.inner().inner()
            .delete_user()
            .account(account)
            .uuid(&user_id)
            .send()
            .await?;

        println!("Deleted user '{}'", user_ref);
    }

    Ok(())
}

async fn resolve_user(id_or_login: &str, client: &AuthenticatedClient) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_login).is_ok() {
        return Ok(id_or_login.to_string());
    }

    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_users()
        .account(account)
        .send()
        .await?;

    let users = response.into_inner();

    for user in &users {
        if user.login == id_or_login {
            return Ok(user.id.to_string());
        }
    }

    Err(anyhow::anyhow!("User not found: {}", id_or_login))
}

// =============================================================================
// Role Implementation
// =============================================================================

async fn list_roles(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_roles()
        .account(account)
        .send()
        .await?;

    let roles = response.into_inner();

    if use_json {
        json::print_json(&roles)?;
    } else {
        let mut tbl = table::create_table(&["UUID", "NAME", "POLICIES", "MEMBERS"]);
        for role in &roles {
            tbl.add_row(vec![
                &role.id.to_string()[..8],
                &role.name,
                &role.policies.as_ref().map(|p| p.join(", ")).unwrap_or_default(),
                &role.members.as_ref().map(|m| m.join(", ")).unwrap_or_default(),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

// Implement remaining role functions similarly...

// =============================================================================
// Policy Implementation
// =============================================================================

async fn list_policies(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_policies()
        .account(account)
        .send()
        .await?;

    let policies = response.into_inner();

    if use_json {
        json::print_json(&policies)?;
    } else {
        let mut tbl = table::create_table(&["UUID", "NAME", "DESCRIPTION"]);
        for policy in &policies {
            tbl.add_row(vec![
                &policy.id.to_string()[..8],
                &policy.name,
                policy.description.as_deref().unwrap_or("-"),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

// Implement remaining policy functions...

// =============================================================================
// Role Tag Implementation
// =============================================================================

async fn set_role_tags(args: RoleTagSetArgs, client: &AuthenticatedClient) -> Result<()> {
    // Implementation depends on resource type
    // Use appropriate PUT endpoint
    println!("Role tags set on {} {}", args.resource_type, args.resource);
    Ok(())
}

async fn get_role_tags(args: RoleTagGetArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    // Implementation depends on resource type
    println!("Role tags for {} {}:", args.resource_type, args.resource);
    Ok(())
}

async fn remove_role_tags(args: RoleTagRemoveArgs, client: &AuthenticatedClient) -> Result<()> {
    // Implementation depends on resource type
    println!("Role tags removed from {} {}", args.resource_type, args.resource);
    Ok(())
}

// Stub implementations for other functions...
async fn get_role(_args: RoleGetArgs, _client: &AuthenticatedClient, _use_json: bool) -> Result<()> { todo!() }
async fn create_role(_args: RoleCreateArgs, _client: &AuthenticatedClient, _use_json: bool) -> Result<()> { todo!() }
async fn update_role(_args: RoleUpdateArgs, _client: &AuthenticatedClient, _use_json: bool) -> Result<()> { todo!() }
async fn delete_roles(_args: RoleDeleteArgs, _client: &AuthenticatedClient) -> Result<()> { todo!() }
async fn get_policy(_args: PolicyGetArgs, _client: &AuthenticatedClient, _use_json: bool) -> Result<()> { todo!() }
async fn create_policy(_args: PolicyCreateArgs, _client: &AuthenticatedClient, _use_json: bool) -> Result<()> { todo!() }
async fn update_policy(_args: PolicyUpdateArgs, _client: &AuthenticatedClient, _use_json: bool) -> Result<()> { todo!() }
async fn delete_policies(_args: PolicyDeleteArgs, _client: &AuthenticatedClient) -> Result<()> { todo!() }
```

### Task 2: Add Top-Level Shortcuts

Update `cli/triton-cli/src/main.rs` to add top-level command shortcuts:

```rust
#[derive(Subcommand)]
enum Commands {
    // Profile management
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    Env { /* ... */ },

    // Information
    Info,

    // Resource management commands
    Instance {
        #[command(subcommand)]
        command: instance::InstanceCommand,
    },
    Image {
        #[command(subcommand)]
        command: image::ImageCommand,
    },
    Key {
        #[command(subcommand)]
        command: key::KeyCommand,
    },
    Network {
        #[command(subcommand)]
        command: network::NetworkCommand,
    },
    Fwrule {
        #[command(subcommand)]
        command: fwrule::FwruleCommand,
    },
    Vlan {
        #[command(subcommand)]
        command: vlan::VlanCommand,
    },
    Volume {
        #[command(subcommand)]
        command: volume::VolumeCommand,
    },
    #[command(alias = "pkg")]
    Package {
        #[command(subcommand)]
        command: package::PackageCommand,
    },
    Account {
        #[command(subcommand)]
        command: account::AccountCommand,
    },
    Rbac {
        #[command(subcommand)]
        command: rbac::RbacCommand,
    },

    // =========================================================================
    // TOP-LEVEL SHORTCUTS
    // =========================================================================

    /// List instances (shortcut for 'instance list')
    #[command(alias = "instances", alias = "insts", alias = "ls")]
    List(instance::list::ListArgs),

    /// Create an instance (shortcut for 'instance create')
    Create(instance::create::CreateArgs),

    /// SSH to an instance (shortcut for 'instance ssh')
    Ssh(instance::ssh::SshArgs),

    /// Start instance(s) (shortcut for 'instance start')
    Start(instance::lifecycle::StartArgs),

    /// Stop instance(s) (shortcut for 'instance stop')
    Stop(instance::lifecycle::StopArgs),

    /// Reboot instance(s) (shortcut for 'instance reboot')
    Reboot(instance::lifecycle::RebootArgs),

    /// Delete instance(s) (shortcut for 'instance delete')
    #[command(alias = "rm")]
    Delete(instance::delete::DeleteArgs),

    /// Get instance IP (shortcut for 'instance ip')
    Ip(instance::get::IpArgs),

    /// List images (shortcut for 'image list')
    #[command(alias = "imgs")]
    Images(image::ImageListArgs),

    /// List packages (shortcut for 'package list')
    #[command(alias = "pkgs")]
    Packages,

    /// List networks (shortcut for 'network list')
    #[command(alias = "nets")]
    Networks,

    /// List volumes (shortcut for 'volume list')
    #[command(alias = "vols")]
    Volumes,

    /// List SSH keys (shortcut for 'key list')
    Keys,

    /// List firewall rules (shortcut for 'fwrule list')
    Fwrules,

    /// List VLANs (shortcut for 'vlan list')
    Vlans,

    /// List datacenters
    #[command(alias = "dcs")]
    Datacenters,

    /// List services
    Services,
}
```

### Task 3: Generate Shell Completions

Add completion command to main.rs:

```rust
use clap::CommandFactory;
use clap_complete::{generate, Shell};

#[derive(Subcommand)]
enum Commands {
    // ... existing commands ...

    /// Generate shell completions
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

// In main():
Commands::Completion { shell } => {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut std::io::stdout());
    Ok(())
}
```

Add to `Cargo.toml`:
```toml
[dependencies]
clap_complete = "4.4"
```

### Task 4: Add Man Page Generation

Add to `Cargo.toml`:
```toml
[build-dependencies]
clap_mangen = "0.2"
```

Create `build.rs`:
```rust
use clap::CommandFactory;
use clap_mangen::Man;
use std::fs;

fn main() {
    // Only generate man pages in release builds
    if std::env::var("PROFILE").unwrap_or_default() != "release" {
        return;
    }

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let man_dir = out_dir.join("man");
    fs::create_dir_all(&man_dir).unwrap();

    // Generate main man page
    let cmd = triton_cli::Cli::command();
    let man = Man::new(cmd);
    let mut buffer = Vec::new();
    man.render(&mut buffer).unwrap();
    fs::write(man_dir.join("triton.1"), buffer).unwrap();
}
```

### Task 5: Add Version and Help Improvements

Update `Cli` struct in main.rs:

```rust
#[derive(Parser)]
#[command(
    name = "triton",
    version,
    about = "Triton cloud management CLI",
    long_about = "User-friendly command-line interface for Triton CloudAPI\n\n\
        Configuration:\n\
        - Use 'triton profile create' to set up a connection profile\n\
        - Or set TRITON_URL, TRITON_ACCOUNT, TRITON_KEY_ID environment variables\n\n\
        For more information, see: https://docs.tritondatacenter.com/",
    after_help = "Examples:\n\
        triton profile create              # Interactive profile setup\n\
        triton instance list               # List all instances\n\
        triton instance create img pkg     # Create a new instance\n\
        triton ssh <instance>              # SSH to an instance",
    propagate_version = true,
)]
struct Cli {
    // ...
}
```

### Task 6: Add Error Handling Improvements

Create `cli/triton-cli/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CliError {
    #[error("Authentication failed: {0}")]
    AuthError(#[from] triton_auth::AuthError),

    #[error("API error: {0}")]
    ApiError(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::AuthError(_) => 2,
            Self::NotFound(_) => 3,
            Self::InvalidInput(_) => 4,
            Self::ConfigError(_) => 5,
            Self::ApiError(_) => 6,
            Self::NetworkError(_) => 7,
            Self::Other(_) => 1,
        }
    }
}

/// Handle errors and exit with appropriate code
pub fn handle_error(err: CliError) {
    eprintln!("Error: {}", err);
    std::process::exit(err.exit_code());
}
```

Update main() error handling:

```rust
fn main() {
    let result = run();
    if let Err(err) = result {
        error::handle_error(err);
    }
}

fn run() -> Result<(), error::CliError> {
    // ... CLI logic ...
}
```

### Task 7: Add Progress Indicators

For long-running operations, add progress bars:

```rust
use indicatif::{ProgressBar, ProgressStyle};

fn create_spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap()
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    pb
}

// Usage in wait operations:
async fn wait_for_state(...) -> Result<()> {
    let pb = create_spinner("Waiting for instance...");
    // ... polling loop ...
    pb.finish_with_message("Done");
}
```

### Task 8: Add Configuration Validation

Create `cli/triton-cli/src/config/validation.rs`:

```rust
use crate::config::Profile;
use anyhow::Result;

/// Validate a profile configuration
pub fn validate_profile(profile: &Profile) -> Result<()> {
    // Validate URL
    if !profile.url.starts_with("http://") && !profile.url.starts_with("https://") {
        return Err(anyhow::anyhow!("URL must start with http:// or https://"));
    }

    // Validate key ID format (MD5 fingerprint)
    let parts: Vec<&str> = profile.key_id.split(':').collect();
    if parts.len() != 16 {
        return Err(anyhow::anyhow!(
            "Invalid key fingerprint format. Expected MD5 format: aa:bb:cc:..."
        ));
    }
    for part in &parts {
        if part.len() != 2 || !part.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!("Invalid key fingerprint format"));
        }
    }

    // Validate account name
    if profile.account.is_empty() {
        return Err(anyhow::anyhow!("Account name cannot be empty"));
    }

    Ok(())
}

/// Test connection with a profile
pub async fn test_profile(profile: &Profile) -> Result<()> {
    use cloudapi_client::AuthenticatedClient;

    let auth_state = profile.to_auth_state();
    let client = AuthenticatedClient::new(&profile.url, auth_state);

    // Try to get account info
    client.inner().inner()
        .get_account()
        .account(&profile.account)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Connection test failed: {}", e))?;

    Ok(())
}
```

### Task 9: Write Tests

Create `cli/triton-cli/tests/`:

```rust
// tests/profile_test.rs
use triton_cli::config::{Profile, Config};
use tempfile::tempdir;

#[test]
fn test_profile_create_and_load() {
    let dir = tempdir().unwrap();
    std::env::set_var("TRITON_CONFIG_DIR", dir.path());

    let profile = Profile::new(
        "test".to_string(),
        "https://cloudapi.example.com".to_string(),
        "testaccount".to_string(),
        "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99".to_string(),
    );

    profile.save().unwrap();

    let loaded = Profile::load("test").unwrap();
    assert_eq!(loaded.name, "test");
    assert_eq!(loaded.url, "https://cloudapi.example.com");
}

#[test]
fn test_profile_list() {
    let dir = tempdir().unwrap();
    std::env::set_var("TRITON_CONFIG_DIR", dir.path());

    // Create multiple profiles
    for name in ["alpha", "beta", "gamma"] {
        let profile = Profile::new(
            name.to_string(),
            "https://cloudapi.example.com".to_string(),
            "account".to_string(),
            "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99".to_string(),
        );
        profile.save().unwrap();
    }

    let profiles = Profile::list_all().unwrap();
    assert_eq!(profiles.len(), 3);
    assert!(profiles.contains(&"alpha".to_string()));
}
```

## Verification

After completing all tasks:

1. Run `cargo build -p triton-cli` - should compile
2. Run `cargo test -p triton-cli` - tests should pass
3. Test RBAC commands:
   ```bash
   ./target/debug/triton rbac user list
   ./target/debug/triton rbac role list
   ./target/debug/triton rbac policy list
   ```
4. Test shortcuts:
   ```bash
   ./target/debug/triton ls                    # instance list
   ./target/debug/triton ssh <instance>        # instance ssh
   ./target/debug/triton images                # image list
   ```
5. Test completions:
   ```bash
   ./target/debug/triton completion bash > /tmp/triton.bash
   source /tmp/triton.bash
   triton <TAB>
   ```

## Files Created

- `cli/triton-cli/src/commands/rbac.rs`
- `cli/triton-cli/src/error.rs`
- `cli/triton-cli/src/config/validation.rs`
- `cli/triton-cli/tests/profile_test.rs`
- `cli/triton-cli/build.rs` (for man pages)

## Modified Files

- `cli/triton-cli/Cargo.toml`
- `cli/triton-cli/src/main.rs`
- `cli/triton-cli/src/commands/mod.rs`

## Summary

At the end of Phase 4, the triton CLI will have:

1. **Complete RBAC support**: User, role, and policy management with role tags
2. **Top-level shortcuts**: Common operations accessible without subcommands
3. **Shell completions**: bash, zsh, fish, powershell
4. **Man pages**: Auto-generated documentation
5. **Improved error handling**: Exit codes, user-friendly messages
6. **Progress indicators**: Spinners for long-running operations
7. **Configuration validation**: Profile testing and validation
8. **Comprehensive tests**: Unit and integration tests

The CLI will have full feature parity with node-triton for Phase 1 requirements.
