<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Phase 2: Instance Commands

## Goal

Implement instance management commands with full feature parity with node-triton.

## Prerequisites

- Phase 0 complete (triton-auth library working)
- Phase 1 complete (basic CLI structure, profiles working)

## Reference

- `target/node-triton/lib/do_instance/` - Complete instance subcommand implementation
- `target/node-triton/lib/do_instances.js` - Instance list shortcut

## Tasks

### Task 1: Create Instance Commands Directory Structure

```
cli/triton-cli/src/commands/instance/
├── mod.rs           # Instance subcommand routing
├── list.rs          # list/ls
├── get.rs           # get
├── create.rs        # create
├── delete.rs        # delete/rm
├── lifecycle.rs     # start/stop/reboot
├── resize.rs        # resize
├── rename.rs        # rename
├── ssh.rs           # ssh
├── wait.rs          # wait
├── audit.rs         # audit
├── firewall.rs      # enable-firewall, disable-firewall, fwrules
├── protection.rs    # enable-deletion-protection, disable-deletion-protection
├── nic.rs           # nic subcommands
├── snapshot.rs      # snapshot subcommands
├── disk.rs          # disk subcommands
├── tag.rs           # tag subcommands
└── metadata.rs      # metadata subcommands
```

### Task 2: Implement Instance Module (`commands/instance/mod.rs`)

```rust
//! Instance management commands

use anyhow::Result;
use clap::Subcommand;

mod audit;
mod create;
mod delete;
mod disk;
mod firewall;
mod get;
mod lifecycle;
mod list;
mod metadata;
mod nic;
mod protection;
mod rename;
mod resize;
mod snapshot;
mod ssh;
mod tag;
mod wait;

#[derive(Subcommand)]
pub enum InstanceCommand {
    /// List instances
    #[command(alias = "ls")]
    List(list::ListArgs),

    /// Get instance details
    Get(get::GetArgs),

    /// Create a new instance
    Create(create::CreateArgs),

    /// Delete instance(s)
    #[command(alias = "rm")]
    Delete(delete::DeleteArgs),

    /// Start instance(s)
    Start(lifecycle::StartArgs),

    /// Stop instance(s)
    Stop(lifecycle::StopArgs),

    /// Reboot instance(s)
    Reboot(lifecycle::RebootArgs),

    /// Resize an instance to a different package
    Resize(resize::ResizeArgs),

    /// Rename an instance
    Rename(rename::RenameArgs),

    /// SSH to an instance
    Ssh(ssh::SshArgs),

    /// Wait for instance state change
    Wait(wait::WaitArgs),

    /// View instance audit log
    Audit(audit::AuditArgs),

    /// Enable firewall for instance(s)
    EnableFirewall(firewall::EnableFirewallArgs),

    /// Disable firewall for instance(s)
    DisableFirewall(firewall::DisableFirewallArgs),

    /// List firewall rules for an instance
    Fwrules(firewall::FwrulesArgs),

    /// Enable deletion protection for instance(s)
    EnableDeletionProtection(protection::EnableProtectionArgs),

    /// Disable deletion protection for instance(s)
    DisableDeletionProtection(protection::DisableProtectionArgs),

    /// Manage instance NICs
    Nic {
        #[command(subcommand)]
        command: nic::NicCommand,
    },

    /// Manage instance snapshots
    Snapshot {
        #[command(subcommand)]
        command: snapshot::SnapshotCommand,
    },

    /// Manage instance disks
    Disk {
        #[command(subcommand)]
        command: disk::DiskCommand,
    },

    /// Manage instance tags
    Tag {
        #[command(subcommand)]
        command: tag::TagCommand,
    },

    /// Manage instance metadata
    Metadata {
        #[command(subcommand)]
        command: metadata::MetadataCommand,
    },

    /// Get instance IP address
    Ip(get::IpArgs),
}

impl InstanceCommand {
    pub async fn run(self, client: &cloudapi_client::AuthenticatedClient, json: bool) -> Result<()> {
        match self {
            Self::List(args) => list::run(args, client, json).await,
            Self::Get(args) => get::run(args, client, json).await,
            Self::Create(args) => create::run(args, client, json).await,
            Self::Delete(args) => delete::run(args, client).await,
            Self::Start(args) => lifecycle::start(args, client).await,
            Self::Stop(args) => lifecycle::stop(args, client).await,
            Self::Reboot(args) => lifecycle::reboot(args, client).await,
            Self::Resize(args) => resize::run(args, client).await,
            Self::Rename(args) => rename::run(args, client).await,
            Self::Ssh(args) => ssh::run(args, client).await,
            Self::Wait(args) => wait::run(args, client, json).await,
            Self::Audit(args) => audit::run(args, client, json).await,
            Self::EnableFirewall(args) => firewall::enable(args, client).await,
            Self::DisableFirewall(args) => firewall::disable(args, client).await,
            Self::Fwrules(args) => firewall::list_rules(args, client, json).await,
            Self::EnableDeletionProtection(args) => protection::enable(args, client).await,
            Self::DisableDeletionProtection(args) => protection::disable(args, client).await,
            Self::Nic { command } => command.run(client, json).await,
            Self::Snapshot { command } => command.run(client, json).await,
            Self::Disk { command } => command.run(client, json).await,
            Self::Tag { command } => command.run(client, json).await,
            Self::Metadata { command } => command.run(client, json).await,
            Self::Ip(args) => get::ip(args, client).await,
        }
    }
}
```

### Task 3: Implement Instance List (`commands/instance/list.rs`)

Reference: `target/node-triton/lib/do_instance/do_list.js`

```rust
use anyhow::Result;
use clap::Args;
use cloudapi_client::{AuthenticatedClient, Machine};
use crate::output::{json, table};

#[derive(Args)]
pub struct ListArgs {
    /// Filter by name (substring match)
    #[arg(long)]
    name: Option<String>,

    /// Filter by state
    #[arg(long)]
    state: Option<String>,

    /// Filter by image
    #[arg(long)]
    image: Option<String>,

    /// Filter by package
    #[arg(long, short = 'p')]
    package: Option<String>,

    /// Filter by tag (key=value)
    #[arg(long, short = 't')]
    tag: Option<Vec<String>>,

    /// Maximum results
    #[arg(long)]
    limit: Option<u32>,

    /// Sort by field
    #[arg(long, default_value = "name")]
    sort_by: String,

    /// Custom output fields
    #[arg(short = 'o', long)]
    output: Option<String>,

    /// Show only short ID
    #[arg(long)]
    short: bool,
}

pub async fn run(args: ListArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;

    let mut req = client.inner().inner().list_machines().account(account);

    if let Some(name) = &args.name {
        req = req.name(name);
    }
    if let Some(state) = &args.state {
        req = req.state(state);
    }
    if let Some(image) = &args.image {
        req = req.image(image);
    }
    if let Some(pkg) = &args.package {
        req = req.package(pkg);
    }
    if let Some(limit) = args.limit {
        req = req.limit(limit as i64);
    }
    // Handle tags
    if let Some(tags) = &args.tag {
        for tag in tags {
            if let Some((key, value)) = tag.split_once('=') {
                req = req.tag(format!("tag.{}={}", key, value));
            }
        }
    }

    let response = req.send().await?;
    let machines = response.into_inner();

    if use_json {
        json::print_json(&machines)?;
    } else {
        print_machines_table(&machines, &args);
    }

    Ok(())
}

fn print_machines_table(machines: &[Machine], args: &ListArgs) {
    let mut tbl = table::create_table(&["SHORTID", "NAME", "IMAGE", "STATE", "PRIMARYIP", "AGE"]);

    for m in machines {
        let short_id = &m.id.to_string()[..8];
        let name = m.name.as_deref().unwrap_or("-");
        let image = &m.image.to_string()[..8];
        let state = format!("{:?}", m.state).to_lowercase();
        let primary_ip = m.primary_ip.as_deref().unwrap_or("-");
        let age = format_age(&m.created);

        tbl.add_row(vec![short_id, name, image, &state, primary_ip, &age]);
    }

    table::print_table(tbl);
}

fn format_age(created: &cloudapi_api::Timestamp) -> String {
    // Calculate age from timestamp
    // Return human-readable format like "2d", "3mo", "1y"
    "".to_string() // TODO: implement
}
```

### Task 4: Implement Instance Get (`commands/instance/get.rs`)

```rust
use anyhow::Result;
use clap::Args;
use cloudapi_client::AuthenticatedClient;
use crate::output::json;

#[derive(Args)]
pub struct GetArgs {
    /// Instance ID or name
    instance: String,
}

#[derive(Args)]
pub struct IpArgs {
    /// Instance ID or name
    instance: String,
}

pub async fn run(args: GetArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let machine = resolve_instance(&args.instance, client).await?;

    let response = client.inner().inner()
        .get_machine()
        .account(account)
        .machine(&machine)
        .send()
        .await?;

    let machine = response.into_inner();

    if use_json {
        json::print_json(&machine)?;
    } else {
        println!("ID:          {}", machine.id);
        println!("Name:        {}", machine.name.as_deref().unwrap_or("-"));
        println!("State:       {:?}", machine.state);
        println!("Image:       {}", machine.image);
        println!("Package:     {}", machine.package);
        println!("Memory:      {} MB", machine.memory);
        println!("Primary IP:  {}", machine.primary_ip.as_deref().unwrap_or("-"));
        println!("Created:     {}", machine.created);
        if machine.firewall_enabled {
            println!("Firewall:    enabled");
        }
        if machine.deletion_protection {
            println!("Deletion Protection: enabled");
        }
    }

    Ok(())
}

pub async fn ip(args: IpArgs, client: &AuthenticatedClient) -> Result<()> {
    let account = &client.auth_state().account;
    let machine_id = resolve_instance(&args.instance, client).await?;

    let response = client.inner().inner()
        .get_machine()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let machine = response.into_inner();

    if let Some(ip) = machine.primary_ip {
        println!("{}", ip);
    } else {
        return Err(anyhow::anyhow!("Instance has no primary IP"));
    }

    Ok(())
}

/// Resolve instance name or short ID to full UUID
pub async fn resolve_instance(id_or_name: &str, client: &AuthenticatedClient) -> Result<String> {
    // First try as UUID
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    // Try as short ID or name
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_machines()
        .account(account)
        .send()
        .await?;

    let machines = response.into_inner();

    // Try short ID match
    for m in &machines {
        if m.id.to_string().starts_with(id_or_name) {
            return Ok(m.id.to_string());
        }
    }

    // Try name match
    for m in &machines {
        if m.name.as_deref() == Some(id_or_name) {
            return Ok(m.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Instance not found: {}", id_or_name))
}
```

### Task 5: Implement Instance Create (`commands/instance/create.rs`)

Reference: `target/node-triton/lib/do_instance/do_create.js` (complex - many options)

```rust
use anyhow::Result;
use clap::Args;
use cloudapi_client::{AuthenticatedClient, CreateMachineRequest};
use std::collections::HashMap;

#[derive(Args)]
pub struct CreateArgs {
    /// Image ID or name@version
    image: String,

    /// Package ID or name
    package: String,

    /// Instance name
    #[arg(long, short)]
    name: Option<String>,

    /// Network IDs (comma-separated or multiple flags)
    #[arg(long, short = 'N')]
    network: Option<Vec<String>>,

    /// Tags (key=value, multiple allowed)
    #[arg(long, short = 't')]
    tag: Option<Vec<String>>,

    /// Metadata (key=value, multiple allowed)
    #[arg(long, short = 'm')]
    metadata: Option<Vec<String>>,

    /// Enable firewall
    #[arg(long)]
    firewall: bool,

    /// Affinity rules
    #[arg(long)]
    affinity: Option<Vec<String>>,

    /// Volume mounts (name:/path)
    #[arg(long)]
    volume: Option<Vec<String>>,

    /// Enable deletion protection
    #[arg(long)]
    deletion_protection: bool,

    /// Wait for instance to be running
    #[arg(long, short)]
    wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    wait_timeout: u64,
}

pub async fn run(args: CreateArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;

    // Resolve image (could be name@version or UUID)
    let image_id = resolve_image(&args.image, client).await?;

    // Resolve package (could be name or UUID)
    let package_id = resolve_package(&args.package, client).await?;

    // Build create request
    let mut request = CreateMachineRequest {
        name: args.name,
        image: image_id,
        package: package_id,
        firewall_enabled: Some(args.firewall),
        deletion_protection: Some(args.deletion_protection),
        ..Default::default()
    };

    // Handle networks
    if let Some(networks) = args.network {
        let network_ids: Vec<String> = networks
            .iter()
            .flat_map(|n| n.split(','))
            .map(|s| s.trim().to_string())
            .collect();
        request.networks = Some(network_ids);
    }

    // Handle tags
    if let Some(tags) = args.tag {
        let mut tag_map = HashMap::new();
        for tag in tags {
            if let Some((key, value)) = tag.split_once('=') {
                tag_map.insert(key.to_string(), value.to_string());
            }
        }
        request.tag = Some(tag_map);
    }

    // Handle metadata
    if let Some(metadata) = args.metadata {
        let mut meta_map = HashMap::new();
        for m in metadata {
            if let Some((key, value)) = m.split_once('=') {
                meta_map.insert(key.to_string(), serde_json::Value::String(value.to_string()));
            }
        }
        // request.metadata = Some(meta_map);
    }

    // Handle affinity
    if let Some(affinity) = args.affinity {
        request.affinity = Some(affinity);
    }

    // Handle volumes
    if let Some(volumes) = args.volume {
        // Parse volume mounts
        // request.volumes = ...
    }

    // Create the instance
    let response = client.inner().inner()
        .create_machine()
        .account(account)
        .body(request)
        .send()
        .await?;

    let machine = response.into_inner();

    println!("Creating instance {} ({})",
        machine.name.as_deref().unwrap_or("unnamed"),
        &machine.id.to_string()[..8]);

    // Wait if requested
    if args.wait {
        println!("Waiting for instance to be running...");
        super::wait::wait_for_state(&machine.id.to_string(), "running", args.wait_timeout, client).await?;
        println!("Instance is running");
    }

    if use_json {
        crate::output::json::print_json(&machine)?;
    }

    Ok(())
}

async fn resolve_image(id_or_name: &str, client: &AuthenticatedClient) -> Result<String> {
    // First try as UUID
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    // Parse name@version format
    let (name, version) = if let Some(idx) = id_or_name.rfind('@') {
        (&id_or_name[..idx], Some(&id_or_name[idx+1..]))
    } else {
        (id_or_name, None)
    };

    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_images()
        .account(account)
        .name(name)
        .send()
        .await?;

    let images = response.into_inner();

    // Find matching image
    for img in &images {
        if let Some(v) = version {
            if img.version.as_deref() == Some(v) {
                return Ok(img.id.to_string());
            }
        } else {
            // Return most recent if no version specified
            return Ok(img.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Image not found: {}", id_or_name))
}

async fn resolve_package(id_or_name: &str, client: &AuthenticatedClient) -> Result<String> {
    // First try as UUID
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_packages()
        .account(account)
        .send()
        .await?;

    let packages = response.into_inner();

    for pkg in &packages {
        if pkg.name == id_or_name {
            return Ok(pkg.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Package not found: {}", id_or_name))
}
```

### Task 6: Implement Lifecycle Commands (`commands/instance/lifecycle.rs`)

```rust
use anyhow::Result;
use clap::Args;
use cloudapi_client::AuthenticatedClient;

#[derive(Args)]
pub struct StartArgs {
    /// Instance ID(s) or name(s)
    instances: Vec<String>,
    /// Wait for instance to be running
    #[arg(long, short)]
    wait: bool,
}

#[derive(Args)]
pub struct StopArgs {
    /// Instance ID(s) or name(s)
    instances: Vec<String>,
    /// Wait for instance to be stopped
    #[arg(long, short)]
    wait: bool,
}

#[derive(Args)]
pub struct RebootArgs {
    /// Instance ID(s) or name(s)
    instances: Vec<String>,
    /// Wait for instance to be running
    #[arg(long, short)]
    wait: bool,
}

pub async fn start(args: StartArgs, client: &AuthenticatedClient) -> Result<()> {
    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let account = &client.auth_state().account;

        client.inner()
            .start_machine(account, &machine_id.parse()?, None)
            .await?;

        println!("Starting instance {}", &machine_id[..8]);

        if args.wait {
            super::wait::wait_for_state(&machine_id, "running", 600, client).await?;
            println!("Instance {} is running", &machine_id[..8]);
        }
    }
    Ok(())
}

pub async fn stop(args: StopArgs, client: &AuthenticatedClient) -> Result<()> {
    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let account = &client.auth_state().account;

        client.inner()
            .stop_machine(account, &machine_id.parse()?, None)
            .await?;

        println!("Stopping instance {}", &machine_id[..8]);

        if args.wait {
            super::wait::wait_for_state(&machine_id, "stopped", 600, client).await?;
            println!("Instance {} is stopped", &machine_id[..8]);
        }
    }
    Ok(())
}

pub async fn reboot(args: RebootArgs, client: &AuthenticatedClient) -> Result<()> {
    for instance in &args.instances {
        let machine_id = super::get::resolve_instance(instance, client).await?;
        let account = &client.auth_state().account;

        client.inner()
            .reboot_machine(account, &machine_id.parse()?, None)
            .await?;

        println!("Rebooting instance {}", &machine_id[..8]);

        if args.wait {
            super::wait::wait_for_state(&machine_id, "running", 600, client).await?;
            println!("Instance {} is running", &machine_id[..8]);
        }
    }
    Ok(())
}
```

### Task 7: Implement SSH Command (`commands/instance/ssh.rs`)

Reference: `target/node-triton/lib/do_instance/do_ssh.js`

```rust
use anyhow::Result;
use clap::Args;
use cloudapi_client::AuthenticatedClient;
use std::process::Command;

#[derive(Args)]
pub struct SshArgs {
    /// Instance ID or name
    instance: String,

    /// SSH user (default: root)
    #[arg(long, short = 'l', default_value = "root")]
    user: String,

    /// SSH identity file
    #[arg(long, short = 'i')]
    identity: Option<String>,

    /// Additional SSH options
    #[arg(long, short = 'o')]
    ssh_option: Option<Vec<String>>,

    /// Command to run on instance
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

pub async fn run(args: SshArgs, client: &AuthenticatedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_state().account;

    // Get instance to find IP
    let response = client.inner().inner()
        .get_machine()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let machine = response.into_inner();

    let ip = machine.primary_ip
        .ok_or_else(|| anyhow::anyhow!("Instance has no primary IP"))?;

    // Build SSH command
    let mut ssh_cmd = Command::new("ssh");

    // Add user@host
    ssh_cmd.arg(format!("{}@{}", args.user, ip));

    // Add identity file if specified
    if let Some(identity) = &args.identity {
        ssh_cmd.arg("-i").arg(identity);
    }

    // Add SSH options
    if let Some(opts) = &args.ssh_option {
        for opt in opts {
            ssh_cmd.arg("-o").arg(opt);
        }
    }

    // Add remote command if specified
    if !args.command.is_empty() {
        ssh_cmd.args(&args.command);
    }

    // Execute SSH
    let status = ssh_cmd.status()?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
```

### Task 8: Implement Wait Command (`commands/instance/wait.rs`)

```rust
use anyhow::Result;
use clap::Args;
use cloudapi_client::AuthenticatedClient;
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[derive(Args)]
pub struct WaitArgs {
    /// Instance ID or name
    instance: String,

    /// Target state(s) to wait for
    #[arg(long, short)]
    state: Option<Vec<String>>,

    /// Timeout in seconds
    #[arg(long, default_value = "600")]
    timeout: u64,
}

pub async fn run(args: WaitArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let states = args.state.unwrap_or_else(|| vec!["running".to_string()]);

    let machine = wait_for_states(&machine_id, &states, args.timeout, client).await?;

    if use_json {
        crate::output::json::print_json(&machine)?;
    } else {
        println!("Instance {} is {:?}", &machine_id[..8], machine.state);
    }

    Ok(())
}

pub async fn wait_for_state(
    machine_id: &str,
    target_state: &str,
    timeout_secs: u64,
    client: &AuthenticatedClient,
) -> Result<()> {
    wait_for_states(machine_id, &[target_state.to_string()], timeout_secs, client).await?;
    Ok(())
}

pub async fn wait_for_states(
    machine_id: &str,
    target_states: &[String],
    timeout_secs: u64,
    client: &AuthenticatedClient,
) -> Result<cloudapi_client::Machine> {
    let account = &client.auth_state().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let response = client.inner().inner()
            .get_machine()
            .account(account)
            .machine(machine_id)
            .send()
            .await?;

        let machine = response.into_inner();
        let current_state = format!("{:?}", machine.state).to_lowercase();

        if target_states.iter().any(|s| s.to_lowercase() == current_state) {
            return Ok(machine);
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for instance to reach state {:?}",
                target_states
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}
```

### Task 9: Implement NIC Subcommands (`commands/instance/nic.rs`)

```rust
use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum NicCommand {
    /// List NICs on an instance
    #[command(alias = "ls")]
    List(NicListArgs),
    /// Get NIC details
    Get(NicGetArgs),
    /// Add a NIC to an instance
    Add(NicAddArgs),
    /// Remove a NIC from an instance
    #[command(alias = "rm")]
    Remove(NicRemoveArgs),
}

#[derive(Args)]
pub struct NicListArgs {
    /// Instance ID or name
    instance: String,
}

#[derive(Args)]
pub struct NicGetArgs {
    /// Instance ID or name
    instance: String,
    /// NIC MAC address
    mac: String,
}

#[derive(Args)]
pub struct NicAddArgs {
    /// Instance ID or name
    instance: String,
    /// Network ID
    #[arg(long)]
    network: String,
}

#[derive(Args)]
pub struct NicRemoveArgs {
    /// Instance ID or name
    instance: String,
    /// NIC MAC address
    mac: String,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
}

impl NicCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_nics(args, client, use_json).await,
            Self::Get(args) => get_nic(args, client, use_json).await,
            Self::Add(args) => add_nic(args, client).await,
            Self::Remove(args) => remove_nic(args, client).await,
        }
    }
}

async fn list_nics(args: NicListArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_state().account;

    let response = client.inner().inner()
        .list_machine_nics()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let nics = response.into_inner();

    if use_json {
        json::print_json(&nics)?;
    } else {
        let mut tbl = table::create_table(&["MAC", "IP", "NETWORK", "PRIMARY"]);
        for nic in &nics {
            tbl.add_row(vec![
                &nic.mac,
                &nic.ip,
                &nic.network.to_string(),
                if nic.primary { "yes" } else { "no" },
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_nic(args: NicGetArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_state().account;

    let response = client.inner().inner()
        .get_machine_nic()
        .account(account)
        .machine(&machine_id)
        .mac(&args.mac)
        .send()
        .await?;

    let nic = response.into_inner();

    if use_json {
        json::print_json(&nic)?;
    } else {
        println!("MAC:     {}", nic.mac);
        println!("IP:      {}", nic.ip);
        println!("Network: {}", nic.network);
        println!("Primary: {}", nic.primary);
    }

    Ok(())
}

async fn add_nic(args: NicAddArgs, client: &AuthenticatedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_state().account;

    let request = cloudapi_client::AddNicRequest {
        network: args.network.parse()?,
    };

    let response = client.inner().inner()
        .add_machine_nic()
        .account(account)
        .machine(&machine_id)
        .body(request)
        .send()
        .await?;

    let nic = response.into_inner();
    println!("Added NIC {} with IP {}", nic.mac, nic.ip);

    Ok(())
}

async fn remove_nic(args: NicRemoveArgs, client: &AuthenticatedClient) -> Result<()> {
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt(format!("Remove NIC {}?", args.mac))
            .default(false)
            .interact()?
        {
            return Ok(());
        }
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_state().account;

    client.inner().inner()
        .remove_machine_nic()
        .account(account)
        .machine(&machine_id)
        .mac(&args.mac)
        .send()
        .await?;

    println!("Removed NIC {}", args.mac);

    Ok(())
}
```

### Task 10: Implement Remaining Instance Subcommands

Create stub implementations for:
- `commands/instance/delete.rs` - Delete instances
- `commands/instance/resize.rs` - Resize to different package
- `commands/instance/rename.rs` - Rename instance
- `commands/instance/audit.rs` - View audit log
- `commands/instance/firewall.rs` - Enable/disable firewall, list rules
- `commands/instance/protection.rs` - Enable/disable deletion protection
- `commands/instance/snapshot.rs` - Snapshot CRUD
- `commands/instance/disk.rs` - Disk CRUD
- `commands/instance/tag.rs` - Tag CRUD
- `commands/instance/metadata.rs` - Metadata CRUD

Follow the same patterns shown above for list/get/add/remove operations.

### Task 11: Update Main CLI to Include Instance Commands

Update `cli/triton-cli/src/main.rs`:

```rust
// Add to Commands enum:
/// Manage instances
#[command(alias = "inst")]
Instance {
    #[command(subcommand)]
    command: commands::instance::InstanceCommand,
},

/// List instances (shortcut)
#[command(alias = "instances")]
Insts(commands::instance::list::ListArgs),

// Top-level shortcuts
/// Create an instance
Create(commands::instance::create::CreateArgs),
/// SSH to an instance
Ssh(commands::instance::ssh::SshArgs),
/// Start instance(s)
Start(commands::instance::lifecycle::StartArgs),
/// Stop instance(s)
Stop(commands::instance::lifecycle::StopArgs),
/// Reboot instance(s)
Reboot(commands::instance::lifecycle::RebootArgs),
/// Delete instance(s)
Delete(commands::instance::delete::DeleteArgs),
```

## Verification

After completing all tasks:

1. Run `make package-build PACKAGE=triton-cli` - should compile
2. Run `make package-test PACKAGE=triton-cli` - tests should pass
3. Test instance commands (requires valid CloudAPI credentials):
   ```bash
   ./target/debug/triton instance list
   ./target/debug/triton instance get <id>
   ./target/debug/triton instance create <image> <package> --name test
   ./target/debug/triton instance start <id>
   ./target/debug/triton instance stop <id>
   ./target/debug/triton instance ssh <id>
   ./target/debug/triton instance nic list <id>
   ./target/debug/triton instance snapshot list <id>
   ```

## Files Created

- `cli/triton-cli/src/commands/instance/mod.rs`
- `cli/triton-cli/src/commands/instance/list.rs`
- `cli/triton-cli/src/commands/instance/get.rs`
- `cli/triton-cli/src/commands/instance/create.rs`
- `cli/triton-cli/src/commands/instance/delete.rs`
- `cli/triton-cli/src/commands/instance/lifecycle.rs`
- `cli/triton-cli/src/commands/instance/resize.rs`
- `cli/triton-cli/src/commands/instance/rename.rs`
- `cli/triton-cli/src/commands/instance/ssh.rs`
- `cli/triton-cli/src/commands/instance/wait.rs`
- `cli/triton-cli/src/commands/instance/audit.rs`
- `cli/triton-cli/src/commands/instance/firewall.rs`
- `cli/triton-cli/src/commands/instance/protection.rs`
- `cli/triton-cli/src/commands/instance/nic.rs`
- `cli/triton-cli/src/commands/instance/snapshot.rs`
- `cli/triton-cli/src/commands/instance/disk.rs`
- `cli/triton-cli/src/commands/instance/tag.rs`
- `cli/triton-cli/src/commands/instance/metadata.rs`

## Modified Files

- `cli/triton-cli/src/main.rs`
- `cli/triton-cli/src/commands/mod.rs`
