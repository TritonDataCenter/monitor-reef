// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance create command

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::output::json;

#[derive(Args, Clone)]
pub struct CreateArgs {
    /// Image ID or name@version
    pub image: String,

    /// Package ID or name
    pub package: String,

    /// Instance name
    #[arg(long, short)]
    pub name: Option<String>,

    /// Network IDs (comma-separated or multiple flags)
    #[arg(long, short = 'N')]
    pub network: Option<Vec<String>>,

    /// Tags (key=value, multiple allowed)
    #[arg(long, short = 't')]
    pub tag: Option<Vec<String>>,

    /// Metadata (key=value, multiple allowed)
    #[arg(long, short = 'm')]
    pub metadata: Option<Vec<String>>,

    /// Metadata from file (key=filepath or key@filepath, multiple allowed).
    /// Reads file contents as the metadata value.
    #[arg(long = "metadata-file", short = 'M')]
    pub metadata_file: Option<Vec<String>>,

    /// User script file (shortcut for -M user-script=FILE)
    #[arg(long)]
    pub script: Option<String>,

    /// Enable firewall
    #[arg(long)]
    pub firewall: bool,

    /// Affinity rules for instance placement.
    /// Format: <key><op><value> where:
    ///   key: 'instance', 'container', or a tag name
    ///   op: '==' (must), '!=' (must not), '==~' (prefer), '!=~' (prefer not)
    ///   value: exact string, glob (*), or regex (/pattern/)
    /// Examples: 'instance==myvm', 'role!=database', 'instance!=~foo*'
    #[arg(long)]
    pub affinity: Option<Vec<String>>,

    /// Enable deletion protection
    #[arg(long)]
    pub deletion_protection: bool,

    /// Instance brand (bhyve, kvm, joyent, joyent-minimal, lx).
    /// If not specified, inferred from the image.
    #[arg(long, short = 'b')]
    pub brand: Option<String>,

    /// Volume to mount (NAME[@MOUNTPOINT] or NAME:MODE:MOUNTPOINT).
    /// MODE can be 'ro' or 'rw' (default: 'rw').
    /// Multiple volumes can be specified.
    #[arg(long, short = 'v')]
    pub volume: Option<Vec<String>>,

    /// Disk specification for bhyve instances (SIZE or IMAGE:SIZE).
    /// SIZE is in MB by default, or use suffixes: G for GB.
    /// Multiple disks can be specified.
    #[arg(long)]
    pub disk: Option<Vec<String>>,

    /// NIC specification (network=UUID[,ip=IP][,primary]).
    /// More advanced than --network, allows specifying IP addresses.
    /// Multiple NICs can be specified.
    #[arg(long)]
    pub nic: Option<Vec<String>>,

    /// Create a delegated ZFS dataset for the zone.
    /// Only applicable to zone-based instances (joyent, joyent-minimal, lx brands).
    #[arg(long)]
    pub delegate_dataset: bool,

    /// Request placement on encrypted compute nodes.
    #[arg(long)]
    pub encrypted: bool,

    /// Allow using images shared with this account (not owned by it).
    #[arg(long)]
    pub allow_shared_images: bool,

    /// Cloud-init config (shortcut for cloud-init user-data metadata).
    /// Can be a file path or inline YAML/JSON content.
    #[arg(long)]
    pub cloud_config: Option<String>,

    /// Simulate creation without actually provisioning (dry-run mode).
    #[arg(long)]
    pub dry_run: bool,

    /// Wait for instance to be running
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

pub async fn run(args: CreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // Resolve image (could be name@version or UUID)
    let image_id = resolve_image(&args.image, client).await?;

    // Resolve package (could be name or UUID)
    let package_id = resolve_package(&args.package, client).await?;

    // Build create request using the builder pattern
    let mut request = cloudapi_client::types::CreateMachineRequest::builder()
        .image(image_id)
        .package(package_id);

    if let Some(name) = &args.name {
        request = request.name(name.clone());
    }
    if args.firewall {
        request = request.firewall_enabled(true);
    }
    if args.deletion_protection {
        request = request.deletion_protection(true);
    }

    // Handle brand
    if let Some(brand) = &args.brand {
        request = request.brand(parse_brand(brand)?);
    }

    // Handle delegate dataset
    if args.delegate_dataset {
        request = request.delegate_dataset(true);
    }

    // Handle encrypted
    if args.encrypted {
        request = request.encrypted(true);
    }

    // Handle allow shared images
    if args.allow_shared_images {
        request = request.allow_shared_images(true);
    }

    // Handle networks (simple mode)
    if let Some(networks) = &args.network {
        let network_ids: Vec<String> = networks
            .iter()
            .flat_map(|n| n.split(','))
            .map(|s| s.trim().to_string())
            .collect();
        request = request.networks(network_ids);
    }

    // Handle NICs (advanced mode)
    if let Some(nics) = &args.nic {
        let nic_specs = parse_nic_specs(nics, client).await?;
        request = request.nics(Some(nic_specs));
    }

    // Handle affinity rules
    if let Some(affinity) = &args.affinity {
        request = request.affinity(affinity.clone());
    }

    // Build metadata from --metadata, --metadata-file, and --script
    let metadata = build_metadata(&args)?;
    if !metadata.is_empty() {
        request = request.metadata(metadata);
    }

    // Handle volumes
    if let Some(volumes) = &args.volume {
        let volume_mounts = parse_volume_specs(volumes)?;
        request = request.volumes(Some(volume_mounts));
    }

    // Handle disks
    if let Some(disks) = &args.disk {
        let disk_specs = parse_disk_specs(disks)?;
        request = request.disks(Some(disk_specs));
    }

    // Build the final request
    let request: cloudapi_client::types::CreateMachineRequest =
        request
            .try_into()
            .map_err(|e: cloudapi_client::types::error::ConversionError| {
                anyhow::anyhow!("Failed to build request: {}", e)
            })?;

    // Handle dry-run mode
    if args.dry_run {
        println!("Dry-run mode: Instance would be created with:");
        if use_json {
            json::print_json(&request)?;
        } else {
            println!("  Image: {:?}", request.image);
            println!("  Package: {}", request.package);
            if let Some(name) = &request.name {
                println!("  Name: {}", name);
            }
            if let Some(brand) = &request.brand {
                println!("  Brand: {:?}", brand);
            }
            if let Some(networks) = &request.networks {
                println!("  Networks: {:?}", networks);
            }
            if let Some(nics) = &request.nics {
                println!("  NICs: {} specified", nics.len());
            }
            if let Some(metadata) = &request.metadata {
                println!("  Metadata keys: {:?}", metadata.keys().collect::<Vec<_>>());
            }
            if let Some(tags) = &request.tags {
                println!("  Tags: {:?}", tags);
            }
            if request.firewall_enabled == Some(true) {
                println!("  Firewall: enabled");
            }
            if request.deletion_protection == Some(true) {
                println!("  Deletion protection: enabled");
            }
            if request.delegate_dataset == Some(true) {
                println!("  Delegate dataset: enabled");
            }
            if request.encrypted == Some(true) {
                println!("  Encrypted: enabled");
            }
            if request.allow_shared_images == Some(true) {
                println!("  Allow shared images: enabled");
            }
            if let Some(volumes) = &request.volumes {
                println!("  Volumes: {} specified", volumes.len());
            }
            if let Some(disks) = &request.disks {
                println!("  Disks: {} specified", disks.len());
            }
        }
        return Ok(());
    }

    // Create the instance
    let response = client
        .inner()
        .create_machine()
        .account(account)
        .body(request)
        .send()
        .await?;

    let machine = response.into_inner();

    println!(
        "Creating instance {} ({})",
        &machine.name,
        &machine.id[..8.min(machine.id.len())]
    );

    // Wait if requested
    if args.wait {
        println!("Waiting for instance to be running...");
        super::wait::wait_for_state(&machine.id, "running", args.wait_timeout, client).await?;
        println!("Instance is running");
    }

    if use_json {
        json::print_json(&machine)?;
    }

    Ok(())
}

/// Parse brand string into Brand enum
fn parse_brand(brand: &str) -> Result<cloudapi_client::types::Brand> {
    match brand.to_lowercase().as_str() {
        "bhyve" => Ok(cloudapi_client::types::Brand::Bhyve),
        "kvm" => Ok(cloudapi_client::types::Brand::Kvm),
        "joyent" => Ok(cloudapi_client::types::Brand::Joyent),
        "joyent-minimal" => Ok(cloudapi_client::types::Brand::JoyentMinimal),
        "lx" => Ok(cloudapi_client::types::Brand::Lx),
        _ => Err(anyhow::anyhow!(
            "Invalid brand '{}'. Valid values: bhyve, kvm, joyent, joyent-minimal, lx",
            brand
        )),
    }
}

/// Build metadata from --metadata, --metadata-file, and --script options
fn build_metadata(args: &CreateArgs) -> Result<HashMap<String, String>> {
    let mut metadata: HashMap<String, String> = HashMap::new();

    // Parse --metadata key=value pairs
    if let Some(metadata_args) = &args.metadata {
        for item in metadata_args {
            let (key, value) = parse_key_value(item)?;
            metadata.insert(key, value);
        }
    }

    // Parse --metadata-file key=filepath or key@filepath
    if let Some(metadata_files) = &args.metadata_file {
        for item in metadata_files {
            let (key, filepath) = parse_key_value_file(item)?;
            let path = Path::new(&filepath);
            if !path.exists() {
                return Err(anyhow::anyhow!("Metadata file not found: {}", filepath));
            }
            let content = fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", filepath, e))?;
            metadata.insert(key, content);
        }
    }

    // Handle --script (shortcut for -M user-script=FILE)
    if let Some(script_path) = &args.script {
        let path = Path::new(script_path);
        if !path.exists() {
            return Err(anyhow::anyhow!("Script file not found: {}", script_path));
        }
        let content = fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", script_path, e))?;
        metadata.insert("user-script".to_string(), content);
    }

    // Handle --cloud-config (shortcut for cloud-init user-data)
    if let Some(cloud_config) = &args.cloud_config {
        let content = if Path::new(cloud_config).exists() {
            // Read from file
            fs::read_to_string(cloud_config)
                .map_err(|e| anyhow::anyhow!("Failed to read cloud-config file: {}", e))?
        } else {
            // Use as inline content
            cloud_config.clone()
        };
        metadata.insert("user-data".to_string(), content);
    }

    Ok(metadata)
}

/// Parse key=value string
fn parse_key_value(s: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = s.splitn(2, '=').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!(
            "Invalid format '{}'. Expected key=value",
            s
        ));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// Parse key=filepath or key@filepath string
fn parse_key_value_file(s: &str) -> Result<(String, String)> {
    // Try key=filepath first
    if let Some(idx) = s.find('=') {
        let (key, filepath) = s.split_at(idx);
        return Ok((key.to_string(), filepath[1..].to_string()));
    }
    // Try key@filepath
    if let Some(idx) = s.find('@') {
        let (key, filepath) = s.split_at(idx);
        return Ok((key.to_string(), filepath[1..].to_string()));
    }
    Err(anyhow::anyhow!(
        "Invalid format '{}'. Expected key=filepath or key@filepath",
        s
    ))
}

/// Parse volume specifications
fn parse_volume_specs(volumes: &[String]) -> Result<Vec<cloudapi_client::types::VolumeMount>> {
    let mut result = Vec::new();
    for spec in volumes {
        let mount = parse_volume_spec(spec)?;
        result.push(mount);
    }
    Ok(result)
}

/// Parse a single volume specification
/// Formats:
///   NAME[@MOUNTPOINT] - mounts at /MOUNTPOINT or /<name>
///   NAME:MODE:MOUNTPOINT - explicit mode and mountpoint
fn parse_volume_spec(spec: &str) -> Result<cloudapi_client::types::VolumeMount> {
    // Try NAME:MODE:MOUNTPOINT format first
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() == 3 {
        let name = parts[0].to_string();
        let mode = parts[1].to_string();
        let mountpoint = parts[2].to_string();

        if mode != "ro" && mode != "rw" {
            return Err(anyhow::anyhow!(
                "Invalid volume mode '{}'. Expected 'ro' or 'rw'",
                mode
            ));
        }

        return Ok(cloudapi_client::types::VolumeMount {
            name,
            mode: Some(mode),
            mountpoint,
            type_: None,
        });
    }

    // Try NAME@MOUNTPOINT or NAME format
    if let Some(idx) = spec.find('@') {
        let name = spec[..idx].to_string();
        let mountpoint = spec[idx + 1..].to_string();
        Ok(cloudapi_client::types::VolumeMount {
            name,
            mode: None, // defaults to "rw"
            mountpoint,
            type_: None,
        })
    } else {
        // Just NAME, use /<name> as mountpoint
        let name = spec.to_string();
        let mountpoint = format!("/{}", name);
        Ok(cloudapi_client::types::VolumeMount {
            name,
            mode: None,
            mountpoint,
            type_: None,
        })
    }
}

/// Parse disk specifications
fn parse_disk_specs(disks: &[String]) -> Result<Vec<cloudapi_client::types::DiskSpec>> {
    let mut result = Vec::new();
    for (i, spec) in disks.iter().enumerate() {
        let disk = parse_disk_spec(spec, i == 0)?;
        result.push(disk);
    }
    Ok(result)
}

/// Parse a single disk specification
/// Formats:
///   SIZE - plain size (e.g., "10240" for 10GB in MB, or "10G" for 10GB)
///   IMAGE:SIZE - image UUID followed by size
fn parse_disk_spec(spec: &str, is_first: bool) -> Result<cloudapi_client::types::DiskSpec> {
    let parts: Vec<&str> = spec.split(':').collect();

    if parts.len() == 2 {
        // IMAGE:SIZE format
        let image = parts[0].to_string();
        let size = parse_size(parts[1])?;
        Ok(cloudapi_client::types::DiskSpec {
            image: Some(image),
            size: Some(size),
            block_size: None,
            boot: if is_first { Some(true) } else { None },
        })
    } else if parts.len() == 1 {
        // SIZE only format
        let size = parse_size(parts[0])?;
        Ok(cloudapi_client::types::DiskSpec {
            image: None,
            size: Some(size),
            block_size: None,
            boot: if is_first { Some(true) } else { None },
        })
    } else {
        Err(anyhow::anyhow!(
            "Invalid disk specification '{}'. Expected SIZE or IMAGE:SIZE",
            spec
        ))
    }
}

/// Parse size string (supports MB or G suffix)
fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.ends_with('G') || s.ends_with('g') {
        let num: u64 = s[..s.len() - 1].parse().map_err(|_| {
            anyhow::anyhow!(
                "Invalid size '{}'. Expected a number with optional G suffix",
                s
            )
        })?;
        Ok(num * 1024) // Convert GB to MB
    } else if s.ends_with('M') || s.ends_with('m') {
        let num: u64 = s[..s.len() - 1].parse().map_err(|_| {
            anyhow::anyhow!(
                "Invalid size '{}'. Expected a number with optional M suffix",
                s
            )
        })?;
        Ok(num)
    } else {
        // Assume MB
        s.parse().map_err(|_| {
            anyhow::anyhow!(
                "Invalid size '{}'. Expected a number (in MB) or with G suffix",
                s
            )
        })
    }
}

/// Parse NIC specifications
async fn parse_nic_specs(
    nics: &[String],
    client: &TypedClient,
) -> Result<Vec<cloudapi_client::types::NicSpec>> {
    let mut result = Vec::new();
    for (i, spec) in nics.iter().enumerate() {
        let nic = parse_nic_spec(spec, i == 0, client).await?;
        result.push(nic);
    }
    Ok(result)
}

/// Parse a single NIC specification
/// Format: network=UUID[,ip=IP][,primary]
async fn parse_nic_spec(
    spec: &str,
    is_first: bool,
    client: &TypedClient,
) -> Result<cloudapi_client::types::NicSpec> {
    let mut network: Option<String> = None;
    let mut ip: Option<String> = None;
    let mut primary: Option<bool> = None;

    for part in spec.split(',') {
        let part = part.trim();
        if part == "primary" {
            primary = Some(true);
        } else if let Some(val) = part.strip_prefix("network=") {
            network = Some(val.to_string());
        } else if let Some(val) = part.strip_prefix("ip=") {
            ip = Some(val.to_string());
        } else if network.is_none() {
            // First value without key is the network
            network = Some(part.to_string());
        } else {
            return Err(anyhow::anyhow!(
                "Invalid NIC specification '{}'. Unknown key '{}'",
                spec,
                part
            ));
        }
    }

    let network_id =
        network.ok_or_else(|| anyhow::anyhow!("NIC specification '{}' missing network", spec))?;

    // Resolve network name to UUID if needed
    let resolved_network = super::super::network::resolve_network(&network_id, client).await?;

    Ok(cloudapi_client::types::NicSpec {
        network: resolved_network,
        ip,
        primary: primary.or(if is_first { Some(true) } else { None }),
        gateway: None,
    })
}

async fn resolve_image(id_or_name: &str, client: &TypedClient) -> Result<String> {
    // First try as UUID
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    // Parse name@version format
    let (name, version) = if let Some(idx) = id_or_name.rfind('@') {
        (&id_or_name[..idx], Some(&id_or_name[idx + 1..]))
    } else {
        (id_or_name, None)
    };

    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_images()
        .account(account)
        .name(name)
        .send()
        .await?;

    let images = response.into_inner();

    // Find matching image
    for img in &images {
        if let Some(v) = version {
            if img.version == v {
                return Ok(img.id.to_string());
            }
        } else {
            // Return first match if no version specified (usually most recent)
            return Ok(img.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Image not found: {}", id_or_name))
}

async fn resolve_package(id_or_name: &str, client: &TypedClient) -> Result<String> {
    // First try as UUID
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    let account = &client.auth_config().account;
    let response = client
        .inner()
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
