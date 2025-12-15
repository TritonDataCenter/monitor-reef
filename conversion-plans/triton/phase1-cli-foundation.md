<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Phase 1: CLI Foundation

## Goal

Create the basic `triton` CLI structure with profile management and environment variable support.

## Prerequisites

- Phase 0 complete (triton-auth library working)

## Tasks

### Task 1: Create `cli/triton-cli` crate structure

**Directory structure:**
```
cli/triton-cli/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── config/
    │   ├── mod.rs
    │   ├── profile.rs
    │   └── paths.rs
    ├── commands/
    │   ├── mod.rs
    │   ├── profile.rs
    │   └── env.rs
    └── output/
        ├── mod.rs
        ├── table.rs
        └── json.rs
```

**Cargo.toml:**
```toml
[package]
name = "triton-cli"
version = "0.1.0"
edition.workspace = true

[[bin]]
name = "triton"
path = "src/main.rs"

[dependencies]
triton-auth = { path = "../../libs/triton-auth" }
cloudapi-client = { path = "../../clients/internal/cloudapi-client" }
cloudapi-api = { path = "../../apis/cloudapi-api" }
clap = { workspace = true, features = ["derive", "env"] }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
thiserror = "2.0"
directories = "5.0"
comfy-table = "7.0"
dialoguer = "0.11"
indicatif = "0.17"
tracing = { workspace = true }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[dev-dependencies]
tempfile = "3.10"
```

**Add to workspace `Cargo.toml` members:**
```toml
"cli/triton-cli",
```

### Task 2: Implement Config Path Resolution (`config/paths.rs`)

Reference: `target/node-triton/lib/config.js:93-118`

```rust
//! Configuration path resolution
//!
//! Supports both legacy ~/.triton/ and XDG ~/.config/triton/ paths.

use std::path::PathBuf;

/// Get the triton configuration directory
///
/// Priority:
/// 1. TRITON_CONFIG_DIR environment variable
/// 2. ~/.triton/ if it exists (migration support)
/// 3. XDG config dir (~/.config/triton/ on Linux/Mac)
pub fn config_dir() -> PathBuf {
    // Check environment variable first
    if let Ok(dir) = std::env::var("TRITON_CONFIG_DIR") {
        return PathBuf::from(dir);
    }

    // Check for existing ~/.triton directory (migration support)
    if let Some(home) = dirs::home_dir() {
        let legacy_dir = home.join(".triton");
        if legacy_dir.exists() {
            return legacy_dir;
        }
    }

    // Default to XDG for new installations
    directories::ProjectDirs::from("com", "tritondatacenter", "triton")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".triton")
        })
}

/// Get the profiles directory
pub fn profiles_dir() -> PathBuf {
    config_dir().join("profiles.d")
}

/// Get the path to the main config file
pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

/// Get the path to a specific profile
pub fn profile_path(name: &str) -> PathBuf {
    profiles_dir().join(format!("{}.json", name))
}

/// Ensure config directories exist
pub fn ensure_config_dirs() -> std::io::Result<()> {
    let config = config_dir();
    let profiles = profiles_dir();

    std::fs::create_dir_all(&config)?;
    std::fs::create_dir_all(&profiles)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_path() {
        let path = profile_path("default");
        assert!(path.ends_with("profiles.d/default.json"));
    }
}
```

### Task 3: Implement Profile Types (`config/profile.rs`)

Reference: `target/node-triton/lib/config.js:59-68`

```rust
//! Profile management types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A connection profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Profile name
    pub name: String,

    /// CloudAPI URL
    pub url: String,

    /// Account login name
    pub account: String,

    /// SSH key fingerprint (MD5 format)
    #[serde(rename = "keyId")]
    pub key_id: String,

    /// Skip TLS certificate verification
    #[serde(default)]
    pub insecure: bool,

    /// RBAC sub-user login (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// RBAC roles to assume (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,

    /// Impersonate another account (optional)
    #[serde(rename = "actAsAccount", skip_serializing_if = "Option::is_none")]
    pub act_as_account: Option<String>,
}

impl Profile {
    /// Create a new profile
    pub fn new(name: String, url: String, account: String, key_id: String) -> Self {
        Self {
            name,
            url,
            account,
            key_id,
            insecure: false,
            user: None,
            roles: None,
            act_as_account: None,
        }
    }

    /// Load a profile from a file
    pub fn load(name: &str) -> anyhow::Result<Self> {
        let path = super::paths::profile_path(name);
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read profile '{}': {}", name, e))?;
        let profile: Profile = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse profile '{}': {}", name, e))?;
        Ok(profile)
    }

    /// Save the profile to a file
    pub fn save(&self) -> anyhow::Result<()> {
        super::paths::ensure_config_dirs()?;
        let path = super::paths::profile_path(&self.name);
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Delete the profile file
    pub fn delete(name: &str) -> anyhow::Result<()> {
        let path = super::paths::profile_path(name);
        std::fs::remove_file(&path)
            .map_err(|e| anyhow::anyhow!("Failed to delete profile '{}': {}", name, e))?;
        Ok(())
    }

    /// List all available profiles
    pub fn list_all() -> anyhow::Result<Vec<String>> {
        let profiles_dir = super::paths::profiles_dir();
        if !profiles_dir.exists() {
            return Ok(vec![]);
        }

        let mut profiles = vec![];
        for entry in std::fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                if let Some(stem) = path.file_stem() {
                    profiles.push(stem.to_string_lossy().to_string());
                }
            }
        }
        profiles.sort();
        Ok(profiles)
    }

    /// Convert to AuthState for triton-auth
    pub fn to_auth_state(&self) -> triton_auth::AuthState {
        let mut state = triton_auth::AuthState::new(
            self.account.clone(),
            self.key_id.clone(),
            triton_auth::KeySource::Auto {
                fingerprint: self.key_id.clone(),
            },
        );

        if let Some(user) = &self.user {
            state = state.with_user(user.clone());
        }

        if let Some(roles) = &self.roles {
            state = state.with_roles(roles.clone());
        }

        state
    }
}

/// Main configuration file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Current active profile name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,

    /// Previous profile (for `triton profile set -`)
    #[serde(rename = "oldProfile", skip_serializing_if = "Option::is_none")]
    pub old_profile: Option<String>,
}

impl Config {
    /// Load the main config file
    pub fn load() -> anyhow::Result<Self> {
        let path = super::paths::config_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    /// Save the main config file
    pub fn save(&self) -> anyhow::Result<()> {
        super::paths::ensure_config_dirs()?;
        let path = super::paths::config_file();
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Get the current profile name
    pub fn current_profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// Set the current profile
    pub fn set_current_profile(&mut self, name: &str) {
        self.old_profile = self.profile.take();
        self.profile = Some(name.to_string());
    }
}
```

### Task 4: Implement Config Module (`config/mod.rs`)

```rust
//! Configuration management

pub mod paths;
pub mod profile;

pub use paths::{config_dir, config_file, ensure_config_dirs, profile_path, profiles_dir};
pub use profile::{Config, Profile};

use anyhow::Result;

/// Build an "env" profile from environment variables
///
/// Reference: node-triton lib/config.js:275-317
pub fn env_profile() -> Result<Profile> {
    let url = std::env::var("TRITON_URL")
        .or_else(|_| std::env::var("SDC_URL"))
        .map_err(|_| anyhow::anyhow!("TRITON_URL or SDC_URL must be set"))?;

    let account = std::env::var("TRITON_ACCOUNT")
        .or_else(|_| std::env::var("SDC_ACCOUNT"))
        .map_err(|_| anyhow::anyhow!("TRITON_ACCOUNT or SDC_ACCOUNT must be set"))?;

    let key_id = std::env::var("TRITON_KEY_ID")
        .or_else(|_| std::env::var("SDC_KEY_ID"))
        .map_err(|_| anyhow::anyhow!("TRITON_KEY_ID or SDC_KEY_ID must be set"))?;

    let mut profile = Profile::new("env".to_string(), url, account, key_id);

    // Optional settings
    if let Ok(user) = std::env::var("TRITON_USER").or_else(|_| std::env::var("SDC_USER")) {
        profile.user = Some(user);
    }

    if let Ok(insecure) = std::env::var("TRITON_TLS_INSECURE")
        .or_else(|_| std::env::var("SDC_TLS_INSECURE"))
    {
        profile.insecure = insecure == "1" || insecure.to_lowercase() == "true";
    }

    Ok(profile)
}

/// Resolve which profile to use
///
/// Priority:
/// 1. CLI --profile argument
/// 2. TRITON_PROFILE environment variable
/// 3. "env" if TRITON_URL/SDC_URL is set (use env vars directly)
/// 4. Current profile from config.json
pub fn resolve_profile(cli_profile: Option<&str>) -> Result<Profile> {
    // 1. CLI argument
    if let Some(name) = cli_profile {
        if name == "env" {
            return env_profile();
        }
        return Profile::load(name);
    }

    // 2. TRITON_PROFILE env var
    if let Ok(name) = std::env::var("TRITON_PROFILE") {
        if name == "env" {
            return env_profile();
        }
        return Profile::load(&name);
    }

    // 3. Check if env vars are set (implicit "env" profile)
    if std::env::var("TRITON_URL").is_ok() || std::env::var("SDC_URL").is_ok() {
        return env_profile();
    }

    // 4. Current profile from config
    let config = Config::load()?;
    if let Some(name) = config.current_profile() {
        return Profile::load(name);
    }

    Err(anyhow::anyhow!(
        "No profile configured. Use 'triton profile create' or set TRITON_* environment variables."
    ))
}
```

### Task 5: Implement Output Formatting (`output/mod.rs`, `output/table.rs`, `output/json.rs`)

**output/mod.rs:**
```rust
//! Output formatting utilities

pub mod json;
pub mod table;

/// Output format selection
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputFormat {
    #[default]
    Table,
    Json,
}
```

**output/table.rs:**
```rust
//! Table output formatting

use comfy_table::{presets::UTF8_FULL, Table};

/// Create a new table with headers
pub fn create_table(headers: &[&str]) -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(headers);
    table
}

/// Format a table and print it
pub fn print_table(table: Table) {
    println!("{table}");
}
```

**output/json.rs:**
```rust
//! JSON output formatting

use serde::Serialize;

/// Print a value as pretty JSON
pub fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    println!("{}", json);
    Ok(())
}
```

### Task 6: Implement Profile Commands (`commands/profile.rs`)

Reference: `target/node-triton/lib/do_profile/`

```rust
//! Profile management commands

use crate::config::{Config, Profile};
use crate::output::{json, table, OutputFormat};
use anyhow::Result;
use clap::Subcommand;
use dialoguer::{Confirm, Input, Password};

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// List all profiles
    #[command(alias = "ls")]
    List {
        /// Output as JSON
        #[arg(short, long)]
        json: bool,
    },

    /// Get current profile details
    Get {
        /// Profile name (defaults to current)
        name: Option<String>,
        /// Output as JSON
        #[arg(short, long)]
        json: bool,
    },

    /// Create a new profile
    Create {
        /// Profile name
        name: Option<String>,
        /// CloudAPI URL
        #[arg(long)]
        url: Option<String>,
        /// Account name
        #[arg(long, short)]
        account: Option<String>,
        /// SSH key fingerprint
        #[arg(long, short)]
        key_id: Option<String>,
        /// Skip TLS verification
        #[arg(long)]
        insecure: bool,
    },

    /// Edit an existing profile
    Edit {
        /// Profile name
        name: String,
    },

    /// Delete a profile
    #[command(alias = "rm")]
    Delete {
        /// Profile name(s)
        names: Vec<String>,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Set the current profile
    SetCurrent {
        /// Profile name (use '-' for previous)
        name: String,
    },
}

impl ProfileCommand {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::List { json: use_json } => list_profiles(use_json),
            Self::Get { name, json: use_json } => get_profile(name, use_json),
            Self::Create { name, url, account, key_id, insecure } => {
                create_profile(name, url, account, key_id, insecure)
            }
            Self::Edit { name } => edit_profile(&name),
            Self::Delete { names, force } => delete_profiles(&names, force),
            Self::SetCurrent { name } => set_current_profile(&name),
        }
    }
}

fn list_profiles(use_json: bool) -> Result<()> {
    let profiles = Profile::list_all()?;
    let config = Config::load()?;
    let current = config.current_profile().unwrap_or("");

    if use_json {
        json::print_json(&profiles)?;
    } else {
        let mut tbl = table::create_table(&["NAME", "CURRENT"]);
        for name in &profiles {
            let marker = if name == current { "*" } else { "" };
            tbl.add_row(vec![name.as_str(), marker]);
        }
        table::print_table(tbl);
    }
    Ok(())
}

fn get_profile(name: Option<String>, use_json: bool) -> Result<()> {
    let profile = match name {
        Some(n) => Profile::load(&n)?,
        None => {
            let config = Config::load()?;
            let current = config.current_profile()
                .ok_or_else(|| anyhow::anyhow!("No current profile set"))?;
            Profile::load(current)?
        }
    };

    if use_json {
        json::print_json(&profile)?;
    } else {
        println!("Name:     {}", profile.name);
        println!("URL:      {}", profile.url);
        println!("Account:  {}", profile.account);
        println!("Key ID:   {}", profile.key_id);
        println!("Insecure: {}", profile.insecure);
        if let Some(user) = &profile.user {
            println!("User:     {}", user);
        }
        if let Some(roles) = &profile.roles {
            println!("Roles:    {}", roles.join(", "));
        }
    }
    Ok(())
}

fn create_profile(
    name: Option<String>,
    url: Option<String>,
    account: Option<String>,
    key_id: Option<String>,
    insecure: bool,
) -> Result<()> {
    // Interactive prompts for missing values
    let name = match name {
        Some(n) => n,
        None => Input::new()
            .with_prompt("Profile name")
            .interact_text()?,
    };

    // Check if profile already exists
    if Profile::list_all()?.contains(&name) {
        return Err(anyhow::anyhow!("Profile '{}' already exists", name));
    }

    let url = match url {
        Some(u) => u,
        None => Input::new()
            .with_prompt("CloudAPI URL")
            .default("https://cloudapi.tritondatacenter.com".to_string())
            .interact_text()?,
    };

    let account = match account {
        Some(a) => a,
        None => Input::new()
            .with_prompt("Account name")
            .interact_text()?,
    };

    let key_id = match key_id {
        Some(k) => k,
        None => Input::new()
            .with_prompt("SSH key fingerprint (aa:bb:cc:...)")
            .interact_text()?,
    };

    let profile = Profile {
        name: name.clone(),
        url,
        account,
        key_id,
        insecure,
        user: None,
        roles: None,
        act_as_account: None,
    };

    profile.save()?;
    println!("Created profile '{}'", name);

    // Ask if this should be the current profile
    if Confirm::new()
        .with_prompt("Set as current profile?")
        .default(true)
        .interact()?
    {
        let mut config = Config::load()?;
        config.set_current_profile(&name);
        config.save()?;
        println!("Set '{}' as current profile", name);
    }

    Ok(())
}

fn edit_profile(name: &str) -> Result<()> {
    let mut profile = Profile::load(name)?;

    profile.url = Input::new()
        .with_prompt("CloudAPI URL")
        .default(profile.url)
        .interact_text()?;

    profile.account = Input::new()
        .with_prompt("Account name")
        .default(profile.account)
        .interact_text()?;

    profile.key_id = Input::new()
        .with_prompt("SSH key fingerprint")
        .default(profile.key_id)
        .interact_text()?;

    profile.insecure = Confirm::new()
        .with_prompt("Skip TLS verification?")
        .default(profile.insecure)
        .interact()?;

    profile.save()?;
    println!("Updated profile '{}'", name);
    Ok(())
}

fn delete_profiles(names: &[String], force: bool) -> Result<()> {
    for name in names {
        if !force {
            if !Confirm::new()
                .with_prompt(format!("Delete profile '{}'?", name))
                .default(false)
                .interact()?
            {
                continue;
            }
        }
        Profile::delete(name)?;
        println!("Deleted profile '{}'", name);
    }
    Ok(())
}

fn set_current_profile(name: &str) -> Result<()> {
    let mut config = Config::load()?;

    let name = if name == "-" {
        config.old_profile.clone()
            .ok_or_else(|| anyhow::anyhow!("No previous profile"))?
    } else {
        // Verify profile exists
        Profile::load(name)?;
        name.to_string()
    };

    config.set_current_profile(&name);
    config.save()?;
    println!("Current profile: {}", name);
    Ok(())
}
```

### Task 7: Implement Env Command (`commands/env.rs`)

Reference: `target/node-triton/lib/do_env.js`

```rust
//! Environment variable export command

use crate::config::{resolve_profile, Profile};
use anyhow::Result;

/// Generate shell export statements for the profile
pub fn generate_env(profile_name: Option<&str>, shell: &str) -> Result<()> {
    let profile = resolve_profile(profile_name)?;

    match shell {
        "bash" | "sh" | "zsh" => print_posix_exports(&profile),
        "fish" => print_fish_exports(&profile),
        "powershell" | "pwsh" => print_powershell_exports(&profile),
        _ => print_posix_exports(&profile),
    }

    Ok(())
}

fn print_posix_exports(profile: &Profile) {
    println!("export TRITON_URL='{}'", profile.url);
    println!("export TRITON_ACCOUNT='{}'", profile.account);
    println!("export TRITON_KEY_ID='{}'", profile.key_id);

    // Legacy SDC_ vars for compatibility
    println!("export SDC_URL='{}'", profile.url);
    println!("export SDC_ACCOUNT='{}'", profile.account);
    println!("export SDC_KEY_ID='{}'", profile.key_id);

    if let Some(user) = &profile.user {
        println!("export TRITON_USER='{}'", user);
        println!("export SDC_USER='{}'", user);
    }

    if profile.insecure {
        println!("export TRITON_TLS_INSECURE='1'");
        println!("export SDC_TLS_INSECURE='1'");
    }

    println!();
    println!("# Run this command to configure your shell:");
    println!("# eval $(triton env)");
}

fn print_fish_exports(profile: &Profile) {
    println!("set -gx TRITON_URL '{}'", profile.url);
    println!("set -gx TRITON_ACCOUNT '{}'", profile.account);
    println!("set -gx TRITON_KEY_ID '{}'", profile.key_id);

    println!("set -gx SDC_URL '{}'", profile.url);
    println!("set -gx SDC_ACCOUNT '{}'", profile.account);
    println!("set -gx SDC_KEY_ID '{}'", profile.key_id);

    if let Some(user) = &profile.user {
        println!("set -gx TRITON_USER '{}'", user);
        println!("set -gx SDC_USER '{}'", user);
    }

    if profile.insecure {
        println!("set -gx TRITON_TLS_INSECURE '1'");
        println!("set -gx SDC_TLS_INSECURE '1'");
    }

    println!();
    println!("# Run this command to configure your shell:");
    println!("# triton env | source");
}

fn print_powershell_exports(profile: &Profile) {
    println!("$env:TRITON_URL = '{}'", profile.url);
    println!("$env:TRITON_ACCOUNT = '{}'", profile.account);
    println!("$env:TRITON_KEY_ID = '{}'", profile.key_id);

    println!("$env:SDC_URL = '{}'", profile.url);
    println!("$env:SDC_ACCOUNT = '{}'", profile.account);
    println!("$env:SDC_KEY_ID = '{}'", profile.key_id);

    if let Some(user) = &profile.user {
        println!("$env:TRITON_USER = '{}'", user);
        println!("$env:SDC_USER = '{}'", user);
    }

    if profile.insecure {
        println!("$env:TRITON_TLS_INSECURE = '1'");
        println!("$env:SDC_TLS_INSECURE = '1'");
    }
}
```

### Task 8: Implement Commands Module (`commands/mod.rs`)

```rust
//! CLI commands

pub mod env;
pub mod profile;

pub use profile::ProfileCommand;
```

### Task 9: Implement Main CLI Entry Point (`main.rs`)

```rust
//! Triton CLI - User-friendly command-line interface for Triton CloudAPI

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
mod config;
mod output;

use commands::ProfileCommand;

#[derive(Parser)]
#[command(
    name = "triton",
    version,
    about = "Triton cloud management CLI",
    long_about = "User-friendly command-line interface for Triton CloudAPI"
)]
struct Cli {
    /// Profile to use
    #[arg(short, long, global = true, env = "TRITON_PROFILE")]
    profile: Option<String>,

    /// CloudAPI URL override
    #[arg(short = 'U', long, global = true, env = "TRITON_URL")]
    url: Option<String>,

    /// Account name override
    #[arg(short, long, global = true, env = "TRITON_ACCOUNT")]
    account: Option<String>,

    /// SSH key fingerprint override
    #[arg(short, long, global = true, env = "TRITON_KEY_ID")]
    key_id: Option<String>,

    /// Output as JSON
    #[arg(short, long, global = true)]
    json: bool,

    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage connection profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },

    /// Generate shell environment exports
    Env {
        /// Profile name (defaults to current)
        profile: Option<String>,
        /// Shell type (bash, fish, powershell)
        #[arg(short, long, default_value = "bash")]
        shell: String,
    },

    // Placeholder for future commands - will be added in subsequent phases
    // Info,
    // Instance { ... },
    // Image { ... },
    // etc.
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up logging
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_env_filter("triton=debug")
            .init();
    }

    match cli.command {
        Commands::Profile { command } => command.run().await,
        Commands::Env { profile, shell } => {
            commands::env::generate_env(profile.as_deref(), &shell)
        }
    }
}
```

## Verification

After completing all tasks:

1. Run `make package-build PACKAGE=triton-cli` - should compile
2. Run `make package-test PACKAGE=triton-cli` - tests should pass
3. Test profile commands:
   ```bash
   ./target/debug/triton profile create
   ./target/debug/triton profile list
   ./target/debug/triton profile get
   ./target/debug/triton env
   ./target/debug/triton profile set-current <name>
   ./target/debug/triton profile delete <name>
   ```
4. Run `make audit` - no new vulnerabilities

## Files Created

- `cli/triton-cli/Cargo.toml`
- `cli/triton-cli/src/main.rs`
- `cli/triton-cli/src/config/mod.rs`
- `cli/triton-cli/src/config/paths.rs`
- `cli/triton-cli/src/config/profile.rs`
- `cli/triton-cli/src/commands/mod.rs`
- `cli/triton-cli/src/commands/profile.rs`
- `cli/triton-cli/src/commands/env.rs`
- `cli/triton-cli/src/output/mod.rs`
- `cli/triton-cli/src/output/table.rs`
- `cli/triton-cli/src/output/json.rs`

## Modified Files

- `Cargo.toml` (workspace members)
