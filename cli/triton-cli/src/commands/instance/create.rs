// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance create command

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

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

    // Handle networks
    if let Some(networks) = &args.network {
        let network_ids: Vec<String> = networks
            .iter()
            .flat_map(|n| n.split(','))
            .map(|s| s.trim().to_string())
            .collect();
        request = request.networks(network_ids);
    }

    // Handle affinity rules
    if let Some(affinity) = &args.affinity {
        request = request.affinity(affinity.clone());
    }

    // Build the final request
    let request: cloudapi_client::types::CreateMachineRequest =
        request
            .try_into()
            .map_err(|e: cloudapi_client::types::error::ConversionError| {
                anyhow::anyhow!("Failed to build request: {}", e)
            })?;

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
