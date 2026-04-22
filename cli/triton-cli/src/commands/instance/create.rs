// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance create command

use anyhow::Result;
use clap::Args;
use cloudapi_api::Machine;
use std::path::Path;

use crate::client::AnyClient;
use crate::output::{enum_to_display, json};
use crate::{dispatch, dispatch_with_types};

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
    #[arg(long, short = 'N', conflicts_with = "nic")]
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
    #[arg(long, short = 'a')]
    pub affinity: Option<Vec<String>>,

    /// Enable deletion protection
    #[arg(long)]
    pub deletion_protection: bool,

    /// Instance brand. If not specified, inferred from the image.
    #[arg(long, short = 'b', value_enum)]
    pub brand: Option<cloudapi_client::types::Brand2>,

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

pub async fn run(
    args: CreateArgs,
    client: &AnyClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();

    // Resolve image (could be name@version or UUID)
    let image_id = crate::commands::image::resolve_image(&args.image, client, cache)
        .await?
        .to_string();

    // Resolve package (could be name or UUID)
    let package_id = resolve_package(&args.package, client).await?;

    // Build the request as a `serde_json::Value`. Node.js CloudAPI accepts
    // the modern tags / metadata objects directly, so we match that wire
    // format and hand the Value off to each per-client Progenitor
    // `types::CreateMachineRequest` via serde round-trip inside the
    // dispatch arm.
    let mut body = serde_json::Map::new();
    body.insert("image".to_string(), serde_json::Value::String(image_id));
    body.insert("package".to_string(), serde_json::Value::String(package_id));

    if let Some(name) = &args.name {
        body.insert("name".to_string(), serde_json::Value::String(name.clone()));
    }
    if args.firewall {
        body.insert(
            "firewall_enabled".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    if args.deletion_protection {
        body.insert(
            "deletion_protection".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    if args.delegate_dataset {
        body.insert(
            "delegate_dataset".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    if args.encrypted {
        body.insert("encrypted".to_string(), serde_json::Value::Bool(true));
    }
    if args.allow_shared_images {
        body.insert(
            "allow_shared_images".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    // Brand: CLI uses Progenitor's `Brand2` (has ValueEnum); serialize to
    // its wire-format string and pass through unchanged.
    if let Some(brand) = &args.brand {
        body.insert("brand".to_string(), serde_json::to_value(brand)?);
    }

    // Handle networks (simple mode - plain UUIDs wrapped as NetworkObject)
    // The pre-hook in cloudapi-client simplifies these to plain UUID strings
    // when no ipv4_ips or primary are set, matching node-triton's wire format.
    let network_objects: Option<Vec<cloudapi_api::NetworkObject>> =
        if let Some(networks) = &args.network {
            let network_strs: Vec<&str> = networks
                .iter()
                .flat_map(|n| n.split(','))
                .map(|s| s.trim())
                .collect();
            let mut list = Vec::new();
            for network_str in network_strs {
                let network_id =
                    super::super::network::resolve_network_with_get(network_str, client).await?;
                list.push(cloudapi_api::NetworkObject {
                    ipv4_uuid: network_id,
                    ipv4_ips: None,
                    primary: None,
                });
            }
            Some(list)
        } else if let Some(nics) = &args.nic {
            Some(parse_nic_specs(nics, client).await?)
        } else {
            None
        };
    if let Some(nets) = &network_objects {
        body.insert("networks".to_string(), serde_json::to_value(nets)?);
    }

    // Handle affinity rules
    if let Some(affinity) = &args.affinity {
        body.insert("affinity".to_string(), serde_json::to_value(affinity)?);
    }

    // Build metadata from --metadata, --metadata-file, and --script
    let metadata = build_metadata(&args).await?;
    if !metadata.is_empty() {
        body.insert("metadata".to_string(), serde_json::Value::Object(metadata));
    }

    // Build tags from --tag
    let tags = build_tags(&args)?;
    if !tags.is_empty() {
        body.insert("tags".to_string(), serde_json::Value::Object(tags));
    }

    // Handle volumes
    let volume_mounts = args
        .volume
        .as_ref()
        .map(|v| parse_volume_specs(v))
        .transpose()?;
    if let Some(mounts) = &volume_mounts {
        body.insert("volumes".to_string(), serde_json::to_value(mounts)?);
    }

    // Handle disks
    let disk_specs = args
        .disk
        .as_ref()
        .map(|d| parse_disk_specs(d))
        .transpose()?;
    if let Some(disks) = &disk_specs {
        body.insert("disks".to_string(), serde_json::to_value(disks)?);
    }

    let body_value = serde_json::Value::Object(body);

    // Handle dry-run mode
    if args.dry_run {
        eprintln!("Dry-run mode: Instance would be created with:");
        if use_json {
            json::print_json(&body_value)?;
        } else {
            if let Some(image) = body_value.get("image").and_then(|v| v.as_str()) {
                println!("  Image: {}", image);
            }
            if let Some(pkg) = body_value.get("package").and_then(|v| v.as_str()) {
                println!("  Package: {}", pkg);
            }
            if let Some(name) = body_value.get("name").and_then(|v| v.as_str()) {
                println!("  Name: {}", name);
            }
            if let Some(brand) = &args.brand {
                println!("  Brand: {}", enum_to_display(brand));
            }
            if let Some(nets) = &network_objects {
                println!("  Networks: {} specified", nets.len());
                for net in nets {
                    let mut desc = net.ipv4_uuid.to_string();
                    if let Some(ips) = &net.ipv4_ips {
                        desc.push_str(&format!(" ({})", ips.join(", ")));
                    }
                    if net.primary == Some(true) {
                        desc.push_str(" [primary]");
                    }
                    println!("    - {}", desc);
                }
            }
            if let Some(md) = body_value.get("metadata").and_then(|v| v.as_object()) {
                println!(
                    "  Metadata keys: {}",
                    md.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            }
            if let Some(tags) = body_value.get("tags").and_then(|v| v.as_object()) {
                println!(
                    "  Tags: {}",
                    tags.iter()
                        .map(|(k, v)| format!("{}={}", k, v))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if args.firewall {
                println!("  Firewall: enabled");
            }
            if args.deletion_protection {
                println!("  Deletion protection: enabled");
            }
            if args.delegate_dataset {
                println!("  Delegate dataset: enabled");
            }
            if args.encrypted {
                println!("  Encrypted: enabled");
            }
            if args.allow_shared_images {
                println!("  Allow shared images: enabled");
            }
            if let Some(mounts) = &volume_mounts {
                println!("  Volumes: {} specified", mounts.len());
            }
            if let Some(disks) = &disk_specs {
                println!("  Disks: {} specified", disks.len());
            }
        }
        return Ok(());
    }

    // Per-client typed `body(V)` takes `V: TryInto<types::CreateMachineRequest>`;
    // serialize the canonical JSON once, deserialize into each arm's type.
    let machine: Machine = dispatch_with_types!(client, |c, t| {
        let body: t::CreateMachineRequest = serde_json::from_value(body_value.clone())?;
        let resp = c
            .inner()
            .create_machine()
            .account(account)
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create machine: {}", e))?
            .into_inner();
        serde_json::from_value::<Machine>(serde_json::to_value(&resp)?)?
    });

    let id_str = machine.id.to_string();
    eprintln!("Creating instance {} ({})", &machine.name, &id_str[..8]);

    // Wait if requested
    if args.wait {
        // Emit initial state immediately so -wj produces NDJSON stream
        if use_json {
            json::print_json(&machine)?;
        }
        eprintln!("Waiting for instance to be running...");
        let (_final_state, final_machine_json) = super::wait::wait_for_states(
            machine.id,
            &[cloudapi_client::types::MachineState::Running],
            args.wait_timeout,
            client,
        )
        .await?;
        eprintln!("Instance is running");
        if use_json {
            json::print_json(&final_machine_json)?;
        }
    } else if use_json {
        json::print_json(&machine)?;
    }

    Ok(())
}

/// Build tags from --tag options
fn build_tags(args: &CreateArgs) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut tags = serde_json::Map::new();

    if let Some(tag_args) = &args.tag {
        for item in tag_args {
            let (key, value) = parse_key_value(item)?;
            // Try to parse as boolean or number, otherwise use as string
            let json_value = if value == "true" {
                serde_json::Value::Bool(true)
            } else if value == "false" {
                serde_json::Value::Bool(false)
            } else if let Ok(num) = value.parse::<i64>() {
                serde_json::Value::Number(num.into())
            } else if let Ok(num) = value.parse::<f64>() {
                serde_json::Number::from_f64(num)
                    .map(serde_json::Value::Number)
                    .unwrap_or_else(|| serde_json::Value::String(value.clone()))
            } else {
                serde_json::Value::String(value)
            };
            tags.insert(key, json_value);
        }
    }

    Ok(tags)
}

/// Build metadata from --metadata, --metadata-file, and --script options
async fn build_metadata(args: &CreateArgs) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut metadata = serde_json::Map::new();

    // Parse --metadata key=value pairs
    if let Some(metadata_args) = &args.metadata {
        for item in metadata_args {
            let (key, value) = parse_key_value(item)?;
            metadata.insert(key, serde_json::Value::String(value));
        }
    }

    // Parse --metadata-file key=filepath or key@filepath
    if let Some(metadata_files) = &args.metadata_file {
        for item in metadata_files {
            let (key, filepath) = parse_key_value_file(item)?;
            let path = Path::new(&filepath);
            if !tokio::fs::try_exists(path).await.unwrap_or(false) {
                return Err(anyhow::anyhow!("Metadata file not found: {}", filepath));
            }
            let content = tokio::fs::read_to_string(path)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", filepath, e))?;
            metadata.insert(key, serde_json::Value::String(content));
        }
    }

    // Handle --script (shortcut for -M user-script=FILE)
    if let Some(script_path) = &args.script {
        let path = Path::new(script_path);
        if !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Err(anyhow::anyhow!("Script file not found: {}", script_path));
        }
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", script_path, e))?;
        metadata.insert(
            "user-script".to_string(),
            serde_json::Value::String(content),
        );
    }

    // Handle --cloud-config (shortcut for cloud-init user-data)
    if let Some(cloud_config) = &args.cloud_config {
        let content = if tokio::fs::try_exists(Path::new(cloud_config))
            .await
            .unwrap_or(false)
        {
            // Read from file
            tokio::fs::read_to_string(cloud_config)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read cloud-config file: {}", e))?
        } else {
            // Use as inline content
            cloud_config.clone()
        };
        metadata.insert("user-data".to_string(), serde_json::Value::String(content));
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
fn parse_volume_specs(volumes: &[String]) -> Result<Vec<cloudapi_api::VolumeMount>> {
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
fn parse_volume_spec(spec: &str) -> Result<cloudapi_api::VolumeMount> {
    // Try NAME:MODE:MOUNTPOINT format first
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() == 3 {
        let name = parts[0].to_string();
        let mode_str = parts[1];
        let mountpoint = parts[2].to_string();

        let mode = match mode_str {
            "rw" => cloudapi_api::MountMode::Rw,
            "ro" => cloudapi_api::MountMode::Ro,
            _ => {
                return Err(anyhow::anyhow!(
                    "Invalid volume mode '{}'. Expected 'ro' or 'rw'",
                    mode_str
                ));
            }
        };

        return Ok(cloudapi_api::VolumeMount {
            name,
            mode: Some(mode),
            mountpoint,
            volume_type: None,
        });
    }

    // Try NAME@MOUNTPOINT or NAME format
    if let Some(idx) = spec.find('@') {
        let name = spec[..idx].to_string();
        let mountpoint = spec[idx + 1..].to_string();
        Ok(cloudapi_api::VolumeMount {
            name,
            mode: None, // defaults to "rw"
            mountpoint,
            volume_type: None,
        })
    } else {
        // Just NAME, use /<name> as mountpoint
        let name = spec.to_string();
        let mountpoint = format!("/{}", name);
        Ok(cloudapi_api::VolumeMount {
            name,
            mode: None,
            mountpoint,
            volume_type: None,
        })
    }
}

/// Parse disk specifications
fn parse_disk_specs(disks: &[String]) -> Result<Vec<cloudapi_api::DiskSpec>> {
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
fn parse_disk_spec(spec: &str, is_first: bool) -> Result<cloudapi_api::DiskSpec> {
    let parts: Vec<&str> = spec.split(':').collect();

    if parts.len() == 2 {
        // IMAGE:SIZE format
        let image: uuid::Uuid = parts[0]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid image UUID in disk spec: {}", parts[0]))?;
        let size = parse_size(parts[1])?;
        Ok(cloudapi_api::DiskSpec {
            image: Some(image),
            size: Some(size),
            block_size: None,
            boot: if is_first { Some(true) } else { None },
        })
    } else if parts.len() == 1 {
        // SIZE only format
        let size = parse_size(parts[0])?;
        Ok(cloudapi_api::DiskSpec {
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

/// Parse NIC specifications into NetworkObject array
async fn parse_nic_specs(
    nics: &[String],
    client: &AnyClient,
) -> Result<Vec<cloudapi_api::NetworkObject>> {
    let mut result = Vec::new();
    for spec in nics {
        let nic = parse_nic_spec(spec, client).await?;
        result.push(nic);
    }
    Ok(result)
}

/// Parsed NIC fields before network resolution
#[derive(Debug)]
struct ParsedNicSpec {
    network: String,
    ip: Option<String>,
    primary: Option<bool>,
}

/// Parse the key=value fields from a NIC specification string.
/// Accepts both native format (network=, ip=) and node-triton format
/// (ipv4_uuid=, ipv4_ips=).
fn parse_nic_spec_fields(spec: &str) -> Result<ParsedNicSpec> {
    let mut network: Option<String> = None;
    let mut ip: Option<String> = None;
    let mut primary: Option<bool> = None;

    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "primary" {
            primary = Some(true);
        } else if let Some(val) = part.strip_prefix("network=") {
            if network.is_some() {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': network specified multiple times",
                    spec
                ));
            }
            if val.is_empty() {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': network value cannot be empty",
                    spec
                ));
            }
            network = Some(val.to_string());
        } else if let Some(val) = part.strip_prefix("ipv4_uuid=") {
            if network.is_some() {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': network specified multiple times (via ipv4_uuid)",
                    spec
                ));
            }
            if val.is_empty() {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': ipv4_uuid value cannot be empty",
                    spec
                ));
            }
            network = Some(val.to_string());
        } else if let Some(val) = part.strip_prefix("ip=") {
            if ip.is_some() {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': IP specified multiple times",
                    spec
                ));
            }
            if val.is_empty() {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': ip value cannot be empty",
                    spec
                ));
            }
            ip = Some(val.to_string());
        } else if let Some(val) = part.strip_prefix("ipv4_ips=") {
            if ip.is_some() {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': IP specified multiple times (via ipv4_ips)",
                    spec
                ));
            }
            if val.is_empty() {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': ipv4_ips value cannot be empty",
                    spec
                ));
            }
            let ips: Vec<&str> = val.split('|').collect();
            if ips.len() != 1 {
                return Err(anyhow::anyhow!(
                    "NIC specification '{}': only 1 ipv4_ip may be specified",
                    spec
                ));
            }
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

    Ok(ParsedNicSpec {
        network: network
            .ok_or_else(|| anyhow::anyhow!("NIC specification '{}' missing network", spec))?,
        ip,
        primary,
    })
}

/// Parse a single NIC specification into a NetworkObject
/// Format: network=UUID[,ip=IP][,primary]
/// Also accepts node-triton format: ipv4_uuid=UUID[,ipv4_ips=IP]
async fn parse_nic_spec(spec: &str, client: &AnyClient) -> Result<cloudapi_api::NetworkObject> {
    let parsed = parse_nic_spec_fields(spec)?;

    // Resolve network name to UUID if needed
    let resolved_network = super::super::network::resolve_network(&parsed.network, client).await?;

    Ok(cloudapi_api::NetworkObject {
        ipv4_uuid: resolved_network,
        ipv4_ips: parsed.ip.map(|ip| vec![ip]),
        primary: parsed.primary,
    })
}

async fn resolve_package(id_or_name: &str, client: &AnyClient) -> Result<String> {
    // First try as full UUID - use parsed form to normalize to lowercase
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        return Ok(uuid.to_string());
    }

    let account = client.effective_account();
    let packages: Vec<(uuid::Uuid, String)> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_packages()
            .account(account)
            .send()
            .await?
            .into_inner();
        resp.into_iter().map(|p| (p.id, p.name)).collect()
    });

    // Check if it looks like a short UUID (hex characters only)
    let is_short_uuid = id_or_name.chars().all(|c| c.is_ascii_hexdigit());

    if is_short_uuid {
        // Find by UUID prefix
        for (id, _) in &packages {
            let pkg_id_str = id.to_string();
            if pkg_id_str.starts_with(id_or_name) {
                return Ok(pkg_id_str);
            }
        }
    } else {
        // Find by name
        for (id, name) in &packages {
            if name == id_or_name {
                return Ok(id.to_string());
            }
        }
    }

    Err(crate::errors::ResourceNotFoundError(format!("Package not found: {}", id_or_name)).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== parse_key_value tests =====

    #[test]
    fn test_parse_key_value_simple() {
        let (key, value) = parse_key_value("foo=bar").unwrap();
        assert_eq!(key, "foo");
        assert_eq!(value, "bar");
    }

    #[test]
    fn test_parse_key_value_with_equals_in_value() {
        let (key, value) = parse_key_value("formula=a=b+c").unwrap();
        assert_eq!(key, "formula");
        assert_eq!(value, "a=b+c");
    }

    #[test]
    fn test_parse_key_value_empty_value() {
        let (key, value) = parse_key_value("empty=").unwrap();
        assert_eq!(key, "empty");
        assert_eq!(value, "");
    }

    #[test]
    fn test_parse_key_value_no_equals() {
        let result = parse_key_value("noequals");
        assert!(result.is_err());
    }

    // ===== parse_key_value_file tests =====

    #[test]
    fn test_parse_key_value_file_equals() {
        let (key, filepath) = parse_key_value_file("user-script=/path/to/script.sh").unwrap();
        assert_eq!(key, "user-script");
        assert_eq!(filepath, "/path/to/script.sh");
    }

    #[test]
    fn test_parse_key_value_file_at() {
        let (key, filepath) = parse_key_value_file("user-script@/path/to/script.sh").unwrap();
        assert_eq!(key, "user-script");
        assert_eq!(filepath, "/path/to/script.sh");
    }

    #[test]
    fn test_parse_key_value_file_no_separator() {
        let result = parse_key_value_file("noseparator");
        assert!(result.is_err());
    }

    // ===== parse_size tests =====

    #[test]
    fn test_parse_size_gb() {
        assert_eq!(parse_size("10G").unwrap(), 10 * 1024);
        assert_eq!(parse_size("10g").unwrap(), 10 * 1024);
    }

    #[test]
    fn test_parse_size_mb_suffix() {
        assert_eq!(parse_size("1024M").unwrap(), 1024);
        assert_eq!(parse_size("1024m").unwrap(), 1024);
    }

    #[test]
    fn test_parse_size_mb_no_suffix() {
        assert_eq!(parse_size("10240").unwrap(), 10240);
    }

    #[test]
    fn test_parse_size_invalid() {
        assert!(parse_size("abc").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_size("10X").is_err());
    }

    // ===== parse_volume_spec tests =====

    #[test]
    fn test_parse_volume_spec_name_only() {
        let mount = parse_volume_spec("myvolume").unwrap();
        assert_eq!(mount.name, "myvolume");
        assert_eq!(mount.mountpoint, "/myvolume");
        assert_eq!(mount.mode, None);
    }

    #[test]
    fn test_parse_volume_spec_name_at_mountpoint() {
        let mount = parse_volume_spec("myvolume@/data").unwrap();
        assert_eq!(mount.name, "myvolume");
        assert_eq!(mount.mountpoint, "/data");
        assert_eq!(mount.mode, None);
    }

    #[test]
    fn test_parse_volume_spec_full() {
        let mount = parse_volume_spec("myvolume:ro:/data").unwrap();
        assert_eq!(mount.name, "myvolume");
        assert_eq!(mount.mountpoint, "/data");
        assert_eq!(mount.mode, Some(cloudapi_api::MountMode::Ro));
    }

    #[test]
    fn test_parse_volume_spec_rw_mode() {
        let mount = parse_volume_spec("myvolume:rw:/data").unwrap();
        assert_eq!(mount.name, "myvolume");
        assert_eq!(mount.mode, Some(cloudapi_api::MountMode::Rw));
    }

    #[test]
    fn test_parse_volume_spec_invalid_mode() {
        let result = parse_volume_spec("myvolume:invalid:/data");
        assert!(result.is_err());
    }

    // ===== parse_disk_spec tests =====

    #[test]
    fn test_parse_disk_spec_size_only() {
        let disk = parse_disk_spec("10240", true).unwrap();
        assert_eq!(disk.size, Some(10240));
        assert_eq!(disk.image, None);
        assert_eq!(disk.boot, Some(true));
    }

    #[test]
    fn test_parse_disk_spec_size_gb() {
        let disk = parse_disk_spec("10G", false).unwrap();
        assert_eq!(disk.size, Some(10 * 1024));
        assert_eq!(disk.boot, None);
    }

    #[test]
    fn test_parse_disk_spec_image_and_size() {
        let disk = parse_disk_spec("12345678-1234-1234-1234-123456789abc:20G", true).unwrap();
        assert_eq!(
            disk.image,
            Some(uuid::Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap())
        );
        assert_eq!(disk.size, Some(20 * 1024));
        assert_eq!(disk.boot, Some(true));
    }

    // ===== parse_nic_spec_fields tests =====

    #[test]
    fn test_parse_nic_spec_network_only() {
        let parsed = parse_nic_spec_fields("48324407-b9c1-40dc-ad11-b0832ecae8ad").unwrap();
        assert_eq!(parsed.network, "48324407-b9c1-40dc-ad11-b0832ecae8ad");
        assert_eq!(parsed.ip, None);
        assert_eq!(parsed.primary, None);
    }

    #[test]
    fn test_parse_nic_spec_network_key() {
        let parsed = parse_nic_spec_fields("network=48324407-b9c1-40dc-ad11-b0832ecae8ad").unwrap();
        assert_eq!(parsed.network, "48324407-b9c1-40dc-ad11-b0832ecae8ad");
        assert_eq!(parsed.primary, None);
    }

    #[test]
    fn test_parse_nic_spec_network_with_ip() {
        let parsed =
            parse_nic_spec_fields("network=48324407-b9c1-40dc-ad11-b0832ecae8ad,ip=192.168.128.75")
                .unwrap();
        assert_eq!(parsed.network, "48324407-b9c1-40dc-ad11-b0832ecae8ad");
        assert_eq!(parsed.ip.as_deref(), Some("192.168.128.75"));
    }

    #[test]
    fn test_parse_nic_spec_node_triton_format() {
        let parsed = parse_nic_spec_fields(
            "ipv4_uuid=48324407-b9c1-40dc-ad11-b0832ecae8ad,ipv4_ips=192.168.128.75",
        )
        .unwrap();
        assert_eq!(parsed.network, "48324407-b9c1-40dc-ad11-b0832ecae8ad");
        assert_eq!(parsed.ip.as_deref(), Some("192.168.128.75"));
        assert_eq!(parsed.primary, None);
    }

    #[test]
    fn test_parse_nic_spec_node_triton_uuid_only() {
        let parsed =
            parse_nic_spec_fields("ipv4_uuid=48324407-b9c1-40dc-ad11-b0832ecae8ad").unwrap();
        assert_eq!(parsed.network, "48324407-b9c1-40dc-ad11-b0832ecae8ad");
        assert_eq!(parsed.ip, None);
    }

    #[test]
    fn test_parse_nic_spec_ipv4_ips_pipe_separated() {
        let err = parse_nic_spec_fields(
            "ipv4_uuid=48324407-b9c1-40dc-ad11-b0832ecae8ad,ipv4_ips=10.0.0.1|10.0.0.2",
        )
        .unwrap_err();
        assert!(err.to_string().contains("only 1 ipv4_ip may be specified"));
        assert!(
            err.to_string().contains(
                "ipv4_uuid=48324407-b9c1-40dc-ad11-b0832ecae8ad,ipv4_ips=10.0.0.1|10.0.0.2"
            )
        );
    }

    #[test]
    fn test_parse_nic_spec_empty_network() {
        let err = parse_nic_spec_fields("network=").unwrap_err();
        assert!(err.to_string().contains("network value cannot be empty"));
    }

    #[test]
    fn test_parse_nic_spec_empty_ipv4_uuid() {
        let err = parse_nic_spec_fields("ipv4_uuid=").unwrap_err();
        assert!(err.to_string().contains("ipv4_uuid value cannot be empty"));
    }

    #[test]
    fn test_parse_nic_spec_empty_ip() {
        let err =
            parse_nic_spec_fields("network=48324407-b9c1-40dc-ad11-b0832ecae8ad,ip=").unwrap_err();
        assert!(err.to_string().contains("ip value cannot be empty"));
    }

    #[test]
    fn test_parse_nic_spec_trailing_comma() {
        let parsed =
            parse_nic_spec_fields("network=48324407-b9c1-40dc-ad11-b0832ecae8ad,").unwrap();
        assert_eq!(parsed.network, "48324407-b9c1-40dc-ad11-b0832ecae8ad");
    }

    #[test]
    fn test_parse_nic_spec_with_primary() {
        let parsed =
            parse_nic_spec_fields("network=48324407-b9c1-40dc-ad11-b0832ecae8ad,primary").unwrap();
        assert_eq!(parsed.primary, Some(true));
    }

    #[test]
    fn test_parse_nic_spec_unknown_key() {
        let result =
            parse_nic_spec_fields("network=48324407-b9c1-40dc-ad11-b0832ecae8ad,bogus=123");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown key"));
    }

    #[test]
    fn test_parse_nic_spec_missing_network() {
        let result = parse_nic_spec_fields("ip=192.168.128.75");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing network"));
    }

    #[test]
    fn test_parse_nic_spec_bare_uuid_not_first() {
        let parsed = parse_nic_spec_fields("48324407-b9c1-40dc-ad11-b0832ecae8ad").unwrap();
        assert_eq!(parsed.network, "48324407-b9c1-40dc-ad11-b0832ecae8ad");
        assert_eq!(parsed.primary, None);
    }

    #[test]
    fn test_parse_nic_spec_conflict_network_ipv4_uuid() {
        let result = parse_nic_spec_fields(
            "network=aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa,ipv4_uuid=bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("network specified multiple times")
        );
    }

    #[test]
    fn test_parse_nic_spec_conflict_ip_ipv4_ips() {
        let result = parse_nic_spec_fields(
            "network=aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa,ip=10.0.0.1,ipv4_ips=10.0.0.2",
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("IP specified multiple times")
        );
    }

    #[test]
    fn test_parse_nic_spec_empty_ipv4_ips() {
        let result =
            parse_nic_spec_fields("network=aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa,ipv4_ips=");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("ipv4_ips value cannot be empty")
        );
    }

    #[test]
    fn test_parse_nic_spec_mixed_format_accepted() {
        // Cross-format mixing of non-conflicting keys is intentionally accepted:
        // ipv4_uuid for network + ip= for address
        let parsed = parse_nic_spec_fields(
            "ipv4_uuid=48324407-b9c1-40dc-ad11-b0832ecae8ad,ip=192.168.128.75",
        )
        .unwrap();
        assert_eq!(parsed.network, "48324407-b9c1-40dc-ad11-b0832ecae8ad");
        assert_eq!(parsed.ip.as_deref(), Some("192.168.128.75"));
    }
}
