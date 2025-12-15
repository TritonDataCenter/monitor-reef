<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Phase 3: Resource Commands

## Goal

Implement commands for images, keys, networks, firewall rules, VLANs, volumes, packages, account, and info.

## Prerequisites

- Phase 0 complete (triton-auth library working)
- Phase 1 complete (basic CLI structure, profiles working)
- Phase 2 complete (instance commands working)

## Tasks

### Task 1: Implement Image Commands (`commands/image.rs`)

Reference: `target/node-triton/lib/do_image/`

```rust
//! Image management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum ImageCommand {
    /// List images
    #[command(alias = "ls")]
    List(ImageListArgs),
    /// Get image details
    Get(ImageGetArgs),
    /// Create image from instance
    Create(ImageCreateArgs),
    /// Delete image
    #[command(alias = "rm")]
    Delete(ImageDeleteArgs),
    /// Clone image to account
    Clone(ImageCloneArgs),
    /// Copy image from another datacenter
    Copy(ImageCopyArgs),
    /// Share image with other accounts
    Share(ImageShareArgs),
    /// Stop sharing image
    Unshare(ImageUnshareArgs),
    /// Update image metadata
    Update(ImageUpdateArgs),
    /// Export image to Manta
    Export(ImageExportArgs),
    /// Wait for image state
    Wait(ImageWaitArgs),
}

#[derive(Args)]
pub struct ImageListArgs {
    /// Filter by name
    #[arg(long)]
    name: Option<String>,
    /// Filter by version
    #[arg(long)]
    version: Option<String>,
    /// Filter by OS
    #[arg(long)]
    os: Option<String>,
    /// Include public images
    #[arg(long)]
    public: bool,
    /// Filter by state
    #[arg(long)]
    state: Option<String>,
    /// Filter by type
    #[arg(long, name = "type")]
    image_type: Option<String>,
}

#[derive(Args)]
pub struct ImageGetArgs {
    /// Image ID or name[@version]
    image: String,
}

#[derive(Args)]
pub struct ImageCreateArgs {
    /// Instance ID or name
    instance: String,
    /// Image name
    #[arg(long)]
    name: String,
    /// Image version
    #[arg(long)]
    version: Option<String>,
    /// Image description
    #[arg(long)]
    description: Option<String>,
    /// Wait for image to be active
    #[arg(long, short)]
    wait: bool,
}

#[derive(Args)]
pub struct ImageDeleteArgs {
    /// Image ID(s) or name[@version]
    images: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
}

#[derive(Args)]
pub struct ImageCloneArgs {
    /// Image ID or name[@version]
    image: String,
}

#[derive(Args)]
pub struct ImageCopyArgs {
    /// Image ID or name[@version] in source datacenter
    image: String,
    /// Source datacenter name
    #[arg(long)]
    source: String,
}

#[derive(Args)]
pub struct ImageShareArgs {
    /// Image ID or name[@version]
    image: String,
    /// Account(s) to share with
    accounts: Vec<String>,
}

#[derive(Args)]
pub struct ImageUnshareArgs {
    /// Image ID or name[@version]
    image: String,
    /// Account(s) to stop sharing with
    accounts: Vec<String>,
}

#[derive(Args)]
pub struct ImageUpdateArgs {
    /// Image ID or name[@version]
    image: String,
    /// New name
    #[arg(long)]
    name: Option<String>,
    /// New version
    #[arg(long)]
    version: Option<String>,
    /// New description
    #[arg(long)]
    description: Option<String>,
}

#[derive(Args)]
pub struct ImageExportArgs {
    /// Image ID or name[@version]
    image: String,
    /// Manta path for export
    #[arg(long)]
    manta_path: String,
}

#[derive(Args)]
pub struct ImageWaitArgs {
    /// Image ID or name[@version]
    image: String,
    /// Target state (default: active)
    #[arg(long, default_value = "active")]
    state: String,
    /// Timeout in seconds
    #[arg(long, default_value = "600")]
    timeout: u64,
}

impl ImageCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_images(args, client, use_json).await,
            Self::Get(args) => get_image(args, client, use_json).await,
            Self::Create(args) => create_image(args, client, use_json).await,
            Self::Delete(args) => delete_images(args, client).await,
            Self::Clone(args) => clone_image(args, client, use_json).await,
            Self::Copy(args) => copy_image(args, client, use_json).await,
            Self::Share(args) => share_image(args, client).await,
            Self::Unshare(args) => unshare_image(args, client).await,
            Self::Update(args) => update_image(args, client, use_json).await,
            Self::Export(args) => export_image(args, client, use_json).await,
            Self::Wait(args) => wait_image(args, client, use_json).await,
        }
    }
}

async fn list_images(args: ImageListArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let mut req = client.inner().inner().list_images().account(account);

    if let Some(name) = &args.name {
        req = req.name(name);
    }
    if let Some(version) = &args.version {
        req = req.version(version);
    }
    if let Some(os) = &args.os {
        req = req.os(os);
    }
    if let Some(state) = &args.state {
        req = req.state(state);
    }
    if args.public {
        req = req.public(true);
    }

    let response = req.send().await?;
    let images = response.into_inner();

    if use_json {
        json::print_json(&images)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "VERSION", "STATE", "TYPE", "OS"]);
        for img in &images {
            tbl.add_row(vec![
                &img.id.to_string()[..8],
                &img.name,
                img.version.as_deref().unwrap_or("-"),
                &format!("{:?}", img.state).to_lowercase(),
                &format!("{:?}", img.r#type).to_lowercase(),
                &img.os,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_image(args: ImageGetArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let image_id = resolve_image(&args.image, client).await?;

    let response = client.inner().inner()
        .get_image()
        .account(account)
        .dataset(&image_id)
        .send()
        .await?;

    let image = response.into_inner();

    if use_json {
        json::print_json(&image)?;
    } else {
        println!("ID:          {}", image.id);
        println!("Name:        {}", image.name);
        println!("Version:     {}", image.version.as_deref().unwrap_or("-"));
        println!("State:       {:?}", image.state);
        println!("Type:        {:?}", image.r#type);
        println!("OS:          {}", image.os);
        if let Some(desc) = &image.description {
            println!("Description: {}", desc);
        }
        println!("Public:      {}", image.public);
    }

    Ok(())
}

async fn create_image(args: ImageCreateArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let machine_id = crate::commands::instance::get::resolve_instance(&args.instance, client).await?;

    let request = cloudapi_client::CreateImageRequest {
        name: args.name,
        version: args.version.unwrap_or_else(|| "1.0.0".to_string()),
        description: args.description,
        machine: machine_id.parse()?,
        ..Default::default()
    };

    let response = client.inner().inner()
        .create_image()
        .account(account)
        .body(request)
        .send()
        .await?;

    let image = response.into_inner();
    println!("Creating image {} ({})", image.name, &image.id.to_string()[..8]);

    if args.wait {
        wait_for_image_state(&image.id.to_string(), "active", 600, client).await?;
        println!("Image is active");
    }

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

// Implement remaining image functions similarly...

async fn resolve_image(id_or_name: &str, client: &AuthenticatedClient) -> Result<String> {
    // UUID check
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    // Parse name@version
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

    for img in &images {
        if let Some(v) = version {
            if img.version.as_deref() == Some(v) {
                return Ok(img.id.to_string());
            }
        } else {
            return Ok(img.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Image not found: {}", id_or_name))
}

async fn wait_for_image_state(
    image_id: &str,
    target_state: &str,
    timeout_secs: u64,
    client: &AuthenticatedClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = &client.auth_state().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let response = client.inner().inner()
            .get_image()
            .account(account)
            .dataset(image_id)
            .send()
            .await?;

        let image = response.into_inner();
        let current_state = format!("{:?}", image.state).to_lowercase();

        if current_state == target_state.to_lowercase() {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for image state: {}", target_state));
        }

        sleep(Duration::from_secs(2)).await;
    }
}
```

### Task 2: Implement Key Commands (`commands/key.rs`)

```rust
//! SSH key management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum KeyCommand {
    /// List SSH keys
    #[command(alias = "ls")]
    List,
    /// Get SSH key details
    Get(KeyGetArgs),
    /// Add SSH key
    Add(KeyAddArgs),
    /// Delete SSH key(s)
    #[command(alias = "rm")]
    Delete(KeyDeleteArgs),
}

#[derive(Args)]
pub struct KeyGetArgs {
    /// Key name or fingerprint
    key: String,
}

#[derive(Args)]
pub struct KeyAddArgs {
    /// Key name
    #[arg(long)]
    name: Option<String>,
    /// Key file path (or read from stdin if not provided)
    file: Option<String>,
}

#[derive(Args)]
pub struct KeyDeleteArgs {
    /// Key name(s) or fingerprint(s)
    keys: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
}

impl KeyCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_keys(client, use_json).await,
            Self::Get(args) => get_key(args, client, use_json).await,
            Self::Add(args) => add_key(args, client, use_json).await,
            Self::Delete(args) => delete_keys(args, client).await,
        }
    }
}

async fn list_keys(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_keys()
        .account(account)
        .send()
        .await?;

    let keys = response.into_inner();

    if use_json {
        json::print_json(&keys)?;
    } else {
        let mut tbl = table::create_table(&["NAME", "FINGERPRINT"]);
        for key in &keys {
            tbl.add_row(vec![&key.name, &key.fingerprint]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_key(args: KeyGetArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .get_key()
        .account(account)
        .name(&args.key)
        .send()
        .await?;

    let key = response.into_inner();

    if use_json {
        json::print_json(&key)?;
    } else {
        println!("Name:        {}", key.name);
        println!("Fingerprint: {}", key.fingerprint);
        println!("Key:         {}", key.key);
    }

    Ok(())
}

async fn add_key(args: KeyAddArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;

    // Read key from file or stdin
    let key_content = if let Some(file) = &args.file {
        std::fs::read_to_string(file)?
    } else {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    };

    // Extract name from key comment if not provided
    let name = args.name.unwrap_or_else(|| {
        key_content
            .split_whitespace()
            .last()
            .unwrap_or("key")
            .to_string()
    });

    let request = cloudapi_client::CreateSshKeyRequest {
        name: name.clone(),
        key: key_content.trim().to_string(),
    };

    let response = client.inner().inner()
        .create_key()
        .account(account)
        .body(request)
        .send()
        .await?;

    let key = response.into_inner();
    println!("Added key '{}' ({})", key.name, key.fingerprint);

    if use_json {
        json::print_json(&key)?;
    }

    Ok(())
}

async fn delete_keys(args: KeyDeleteArgs, client: &AuthenticatedClient) -> Result<()> {
    for key_name in &args.keys {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete key '{}'?", key_name))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        let account = &client.auth_state().account;
        client.inner().inner()
            .delete_key()
            .account(account)
            .name(key_name)
            .send()
            .await?;

        println!("Deleted key '{}'", key_name);
    }

    Ok(())
}
```

### Task 3: Implement Network Commands (`commands/network.rs`)

```rust
//! Network management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum NetworkCommand {
    /// List networks
    #[command(alias = "ls")]
    List,
    /// Get network details
    Get(NetworkGetArgs),
    /// Get default network
    GetDefault,
    /// Set default network
    SetDefault(NetworkSetDefaultArgs),
    /// Manage network IPs
    Ip {
        #[command(subcommand)]
        command: NetworkIpCommand,
    },
}

#[derive(Subcommand)]
pub enum NetworkIpCommand {
    /// List IPs in a network
    #[command(alias = "ls")]
    List(NetworkIpListArgs),
    /// Get IP details
    Get(NetworkIpGetArgs),
    /// Update IP reservation
    Update(NetworkIpUpdateArgs),
}

#[derive(Args)]
pub struct NetworkGetArgs {
    /// Network ID or name
    network: String,
}

#[derive(Args)]
pub struct NetworkSetDefaultArgs {
    /// Network ID or name
    network: String,
}

#[derive(Args)]
pub struct NetworkIpListArgs {
    /// Network ID
    network: String,
}

#[derive(Args)]
pub struct NetworkIpGetArgs {
    /// Network ID
    network: String,
    /// IP address
    ip: String,
}

#[derive(Args)]
pub struct NetworkIpUpdateArgs {
    /// Network ID
    network: String,
    /// IP address
    ip: String,
    /// Reserve the IP
    #[arg(long)]
    reserve: Option<bool>,
}

impl NetworkCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_networks(client, use_json).await,
            Self::Get(args) => get_network(args, client, use_json).await,
            Self::GetDefault => get_default_network(client, use_json).await,
            Self::SetDefault(args) => set_default_network(args, client).await,
            Self::Ip { command } => command.run(client, use_json).await,
        }
    }
}

impl NetworkIpCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_network_ips(args, client, use_json).await,
            Self::Get(args) => get_network_ip(args, client, use_json).await,
            Self::Update(args) => update_network_ip(args, client, use_json).await,
        }
    }
}

async fn list_networks(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_networks()
        .account(account)
        .send()
        .await?;

    let networks = response.into_inner();

    if use_json {
        json::print_json(&networks)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "SUBNET", "GATEWAY", "PUBLIC"]);
        for net in &networks {
            tbl.add_row(vec![
                &net.id.to_string()[..8],
                &net.name,
                net.subnet.as_deref().unwrap_or("-"),
                net.gateway.as_deref().unwrap_or("-"),
                if net.public { "yes" } else { "no" },
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_network(args: NetworkGetArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let network_id = resolve_network(&args.network, client).await?;

    let response = client.inner().inner()
        .get_network()
        .account(account)
        .network(&network_id)
        .send()
        .await?;

    let network = response.into_inner();

    if use_json {
        json::print_json(&network)?;
    } else {
        println!("ID:      {}", network.id);
        println!("Name:    {}", network.name);
        println!("Subnet:  {}", network.subnet.as_deref().unwrap_or("-"));
        println!("Gateway: {}", network.gateway.as_deref().unwrap_or("-"));
        println!("Public:  {}", network.public);
        if let Some(fabric) = network.fabric {
            println!("Fabric:  {}", fabric);
        }
    }

    Ok(())
}

async fn resolve_network(id_or_name: &str, client: &AuthenticatedClient) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_networks()
        .account(account)
        .send()
        .await?;

    let networks = response.into_inner();

    for net in &networks {
        if net.name == id_or_name {
            return Ok(net.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Network not found: {}", id_or_name))
}

// Implement remaining network functions...
```

### Task 4: Implement Firewall Rule Commands (`commands/fwrule.rs`)

```rust
//! Firewall rule management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum FwruleCommand {
    /// List firewall rules
    #[command(alias = "ls")]
    List,
    /// Get firewall rule details
    Get(FwruleGetArgs),
    /// Create firewall rule
    Create(FwruleCreateArgs),
    /// Delete firewall rule(s)
    #[command(alias = "rm")]
    Delete(FwruleDeleteArgs),
    /// Enable firewall rule(s)
    Enable(FwruleEnableArgs),
    /// Disable firewall rule(s)
    Disable(FwruleDisableArgs),
    /// Update firewall rule
    Update(FwruleUpdateArgs),
    /// List instances affected by rule
    Instances(FwruleInstancesArgs),
}

#[derive(Args)]
pub struct FwruleGetArgs {
    /// Rule ID
    id: String,
}

#[derive(Args)]
pub struct FwruleCreateArgs {
    /// Rule text (e.g., "FROM any TO vm <uuid> ALLOW tcp PORT 22")
    rule: String,
    /// Rule description
    #[arg(long)]
    description: Option<String>,
    /// Enable rule (default: true)
    #[arg(long, default_value = "true")]
    enabled: bool,
}

#[derive(Args)]
pub struct FwruleDeleteArgs {
    /// Rule ID(s)
    ids: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
}

#[derive(Args)]
pub struct FwruleEnableArgs {
    /// Rule ID(s)
    ids: Vec<String>,
}

#[derive(Args)]
pub struct FwruleDisableArgs {
    /// Rule ID(s)
    ids: Vec<String>,
}

#[derive(Args)]
pub struct FwruleUpdateArgs {
    /// Rule ID
    id: String,
    /// New rule text
    #[arg(long)]
    rule: Option<String>,
    /// New description
    #[arg(long)]
    description: Option<String>,
}

#[derive(Args)]
pub struct FwruleInstancesArgs {
    /// Rule ID
    id: String,
}

impl FwruleCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_rules(client, use_json).await,
            Self::Get(args) => get_rule(args, client, use_json).await,
            Self::Create(args) => create_rule(args, client, use_json).await,
            Self::Delete(args) => delete_rules(args, client).await,
            Self::Enable(args) => enable_rules(args, client).await,
            Self::Disable(args) => disable_rules(args, client).await,
            Self::Update(args) => update_rule(args, client, use_json).await,
            Self::Instances(args) => list_rule_instances(args, client, use_json).await,
        }
    }
}

async fn list_rules(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_firewall_rules()
        .account(account)
        .send()
        .await?;

    let rules = response.into_inner();

    if use_json {
        json::print_json(&rules)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "ENABLED", "RULE"]);
        for rule in &rules {
            tbl.add_row(vec![
                &rule.id.to_string()[..8],
                if rule.enabled { "yes" } else { "no" },
                &rule.rule,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

// Implement remaining firewall rule functions...
```

### Task 5: Implement VLAN Commands (`commands/vlan.rs`)

```rust
//! Fabric VLAN management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum VlanCommand {
    /// List VLANs
    #[command(alias = "ls")]
    List,
    /// Get VLAN details
    Get(VlanGetArgs),
    /// Create VLAN
    Create(VlanCreateArgs),
    /// Delete VLAN
    #[command(alias = "rm")]
    Delete(VlanDeleteArgs),
    /// Update VLAN
    Update(VlanUpdateArgs),
    /// List networks on VLAN
    Networks(VlanNetworksArgs),
}

#[derive(Args)]
pub struct VlanGetArgs {
    /// VLAN ID
    vlan_id: i32,
}

#[derive(Args)]
pub struct VlanCreateArgs {
    /// VLAN ID (1-4095)
    #[arg(long)]
    vlan_id: i32,
    /// VLAN name
    #[arg(long)]
    name: String,
    /// Description
    #[arg(long)]
    description: Option<String>,
}

#[derive(Args)]
pub struct VlanDeleteArgs {
    /// VLAN ID(s)
    vlan_ids: Vec<i32>,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
}

#[derive(Args)]
pub struct VlanUpdateArgs {
    /// VLAN ID
    vlan_id: i32,
    /// New name
    #[arg(long)]
    name: Option<String>,
    /// New description
    #[arg(long)]
    description: Option<String>,
}

#[derive(Args)]
pub struct VlanNetworksArgs {
    /// VLAN ID
    vlan_id: i32,
}

impl VlanCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_vlans(client, use_json).await,
            Self::Get(args) => get_vlan(args, client, use_json).await,
            Self::Create(args) => create_vlan(args, client, use_json).await,
            Self::Delete(args) => delete_vlans(args, client).await,
            Self::Update(args) => update_vlan(args, client, use_json).await,
            Self::Networks(args) => list_vlan_networks(args, client, use_json).await,
        }
    }
}

async fn list_vlans(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_fabric_vlans()
        .account(account)
        .send()
        .await?;

    let vlans = response.into_inner();

    if use_json {
        json::print_json(&vlans)?;
    } else {
        let mut tbl = table::create_table(&["VLAN_ID", "NAME", "DESCRIPTION"]);
        for vlan in &vlans {
            tbl.add_row(vec![
                &vlan.vlan_id.to_string(),
                &vlan.name,
                vlan.description.as_deref().unwrap_or("-"),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

// Implement remaining VLAN functions...
```

### Task 6: Implement Volume Commands (`commands/volume.rs`)

```rust
//! Volume management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum VolumeCommand {
    /// List volumes
    #[command(alias = "ls")]
    List,
    /// Get volume details
    Get(VolumeGetArgs),
    /// Create volume
    Create(VolumeCreateArgs),
    /// Delete volume(s)
    #[command(alias = "rm")]
    Delete(VolumeDeleteArgs),
    /// List available volume sizes
    Sizes,
}

#[derive(Args)]
pub struct VolumeGetArgs {
    /// Volume ID or name
    volume: String,
}

#[derive(Args)]
pub struct VolumeCreateArgs {
    /// Volume name
    #[arg(long)]
    name: String,
    /// Volume size (e.g., "10G")
    #[arg(long)]
    size: String,
    /// Volume type
    #[arg(long, default_value = "tritonnfs")]
    r#type: String,
    /// Network IDs
    #[arg(long)]
    network: Option<Vec<String>>,
}

#[derive(Args)]
pub struct VolumeDeleteArgs {
    /// Volume ID(s) or name(s)
    volumes: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    force: bool,
    /// Wait for deletion
    #[arg(long, short)]
    wait: bool,
}

impl VolumeCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_volumes(client, use_json).await,
            Self::Get(args) => get_volume(args, client, use_json).await,
            Self::Create(args) => create_volume(args, client, use_json).await,
            Self::Delete(args) => delete_volumes(args, client).await,
            Self::Sizes => list_volume_sizes(client, use_json).await,
        }
    }
}

async fn list_volumes(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_volumes()
        .account(account)
        .send()
        .await?;

    let volumes = response.into_inner();

    if use_json {
        json::print_json(&volumes)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "SIZE", "STATE", "TYPE"]);
        for vol in &volumes {
            tbl.add_row(vec![
                &vol.id.to_string()[..8],
                &vol.name,
                &format!("{} MB", vol.size),
                &format!("{:?}", vol.state).to_lowercase(),
                &vol.r#type,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

// Implement remaining volume functions...
```

### Task 7: Implement Package Commands (`commands/package.rs`)

```rust
//! Package management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::{json, table};

#[derive(Subcommand)]
pub enum PackageCommand {
    /// List packages
    #[command(alias = "ls")]
    List,
    /// Get package details
    Get(PackageGetArgs),
}

#[derive(Args)]
pub struct PackageGetArgs {
    /// Package ID or name
    package: String,
}

impl PackageCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_packages(client, use_json).await,
            Self::Get(args) => get_package(args, client, use_json).await,
        }
    }
}

async fn list_packages(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .list_packages()
        .account(account)
        .send()
        .await?;

    let packages = response.into_inner();

    if use_json {
        json::print_json(&packages)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "MEMORY", "DISK", "VCPUS"]);
        for pkg in &packages {
            tbl.add_row(vec![
                &pkg.id.to_string()[..8],
                &pkg.name,
                &format!("{} MB", pkg.memory),
                &format!("{} MB", pkg.disk),
                &pkg.vcpus.map(|v| v.to_string()).unwrap_or("-".to_string()),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_package(args: PackageGetArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let package_id = resolve_package(&args.package, client).await?;

    let response = client.inner().inner()
        .get_package()
        .account(account)
        .package(&package_id)
        .send()
        .await?;

    let package = response.into_inner();

    if use_json {
        json::print_json(&package)?;
    } else {
        println!("ID:     {}", package.id);
        println!("Name:   {}", package.name);
        println!("Memory: {} MB", package.memory);
        println!("Disk:   {} MB", package.disk);
        if let Some(vcpus) = package.vcpus {
            println!("vCPUs:  {}", vcpus);
        }
    }

    Ok(())
}

async fn resolve_package(id_or_name: &str, client: &AuthenticatedClient) -> Result<String> {
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

### Task 8: Implement Account Commands (`commands/account.rs`)

```rust
//! Account management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::AuthenticatedClient;
use crate::output::json;

#[derive(Subcommand)]
pub enum AccountCommand {
    /// Get account details
    Get,
    /// Get account resource limits
    Limits,
    /// Update account settings
    Update(AccountUpdateArgs),
}

#[derive(Args)]
pub struct AccountUpdateArgs {
    /// New email
    #[arg(long)]
    email: Option<String>,
    /// Given name
    #[arg(long)]
    given_name: Option<String>,
    /// Surname
    #[arg(long)]
    surname: Option<String>,
    /// Company name
    #[arg(long)]
    company_name: Option<String>,
    /// Phone number
    #[arg(long)]
    phone: Option<String>,
}

impl AccountCommand {
    pub async fn run(self, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Get => get_account(client, use_json).await,
            Self::Limits => get_limits(client, use_json).await,
            Self::Update(args) => update_account(args, client, use_json).await,
        }
    }
}

async fn get_account(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .get_account()
        .account(account)
        .send()
        .await?;

    let acc = response.into_inner();

    if use_json {
        json::print_json(&acc)?;
    } else {
        println!("Login:     {}", acc.login);
        println!("Email:     {}", acc.email);
        if let Some(name) = &acc.first_name {
            println!("Name:      {} {}", name, acc.last_name.as_deref().unwrap_or(""));
        }
        if let Some(company) = &acc.company_name {
            println!("Company:   {}", company);
        }
    }

    Ok(())
}

async fn get_limits(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;
    let response = client.inner().inner()
        .get_provisioning_limits()
        .account(account)
        .send()
        .await?;

    let limits = response.into_inner();

    if use_json {
        json::print_json(&limits)?;
    } else {
        println!("Provisioning Limits:");
        // Display limits in readable format
    }

    Ok(())
}

async fn update_account(args: AccountUpdateArgs, client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;

    let request = cloudapi_client::UpdateAccountRequest {
        email: args.email,
        first_name: args.given_name,
        last_name: args.surname,
        company_name: args.company_name,
        phone: args.phone,
        ..Default::default()
    };

    let response = client.inner().inner()
        .update_account()
        .account(account)
        .body(request)
        .send()
        .await?;

    let acc = response.into_inner();
    println!("Account updated");

    if use_json {
        json::print_json(&acc)?;
    }

    Ok(())
}
```

### Task 9: Implement Info Command (`commands/info.rs`)

```rust
//! Account info/overview command

use anyhow::Result;
use cloudapi_client::AuthenticatedClient;

pub async fn run(client: &AuthenticatedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_state().account;

    // Fetch account details
    let acc_response = client.inner().inner()
        .get_account()
        .account(account)
        .send()
        .await?;
    let acc = acc_response.into_inner();

    // Fetch machines
    let machines_response = client.inner().inner()
        .list_machines()
        .account(account)
        .send()
        .await?;
    let machines = machines_response.into_inner();

    // Calculate stats
    let running = machines.iter().filter(|m| format!("{:?}", m.state) == "Running").count();
    let stopped = machines.iter().filter(|m| format!("{:?}", m.state) == "Stopped").count();
    let total_memory: i64 = machines.iter().map(|m| m.memory).sum();
    let total_disk: i64 = machines.iter().map(|m| m.disk).sum();

    if use_json {
        let info = serde_json::json!({
            "login": acc.login,
            "instances": {
                "total": machines.len(),
                "running": running,
                "stopped": stopped,
            },
            "memory_used_mb": total_memory,
            "disk_used_mb": total_disk,
        });
        crate::output::json::print_json(&info)?;
    } else {
        println!("Account: {}", acc.login);
        println!();
        println!("Instances:");
        println!("  Total:   {}", machines.len());
        println!("  Running: {}", running);
        println!("  Stopped: {}", stopped);
        println!();
        println!("Resources:");
        println!("  Memory:  {} MB", total_memory);
        println!("  Disk:    {} MB", total_disk);
    }

    Ok(())
}
```

### Task 10: Update Main CLI and Commands Module

Update `cli/triton-cli/src/commands/mod.rs`:
```rust
pub mod account;
pub mod env;
pub mod fwrule;
pub mod image;
pub mod info;
pub mod instance;
pub mod key;
pub mod network;
pub mod package;
pub mod profile;
pub mod vlan;
pub mod volume;
```

Update `cli/triton-cli/src/main.rs` to add all new commands to the Commands enum.

## Verification

After completing all tasks:

1. Run `cargo build -p triton-cli` - should compile
2. Run `cargo test -p triton-cli` - tests should pass
3. Test resource commands (requires valid CloudAPI credentials):
   ```bash
   ./target/debug/triton image list
   ./target/debug/triton key list
   ./target/debug/triton network list
   ./target/debug/triton fwrule list
   ./target/debug/triton vlan list
   ./target/debug/triton volume list
   ./target/debug/triton package list
   ./target/debug/triton account get
   ./target/debug/triton info
   ```

## Files Created

- `cli/triton-cli/src/commands/image.rs`
- `cli/triton-cli/src/commands/key.rs`
- `cli/triton-cli/src/commands/network.rs`
- `cli/triton-cli/src/commands/fwrule.rs`
- `cli/triton-cli/src/commands/vlan.rs`
- `cli/triton-cli/src/commands/volume.rs`
- `cli/triton-cli/src/commands/package.rs`
- `cli/triton-cli/src/commands/account.rs`
- `cli/triton-cli/src/commands/info.rs`

## Modified Files

- `cli/triton-cli/src/main.rs`
- `cli/triton-cli/src/commands/mod.rs`
