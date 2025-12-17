// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Image management commands

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use cloudapi_client::types::Image;
use dialoguer::Confirm;
use serde_json::{Map, Value};

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
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
    /// Update image metadata
    Update(ImageUpdateArgs),
    /// Export image to Manta
    Export(ImageExportArgs),
    /// Share image with another account
    Share(ImageShareArgs),
    /// Unshare image from another account
    Unshare(ImageUnshareArgs),
    /// Manage image tags
    Tag {
        #[command(subcommand)]
        command: ImageTagCommand,
    },
    /// Wait for image state
    Wait(ImageWaitArgs),
}

#[derive(Subcommand, Clone)]
pub enum ImageTagCommand {
    /// List tags on an image
    #[command(alias = "ls")]
    List(ImageTagListArgs),

    /// Get a tag value
    Get(ImageTagGetArgs),

    /// Set tag(s) on an image
    Set(ImageTagSetArgs),

    /// Delete a tag from an image
    #[command(alias = "rm")]
    Delete(ImageTagDeleteArgs),
}

#[derive(Args, Clone)]
pub struct ImageListArgs {
    /// Filter by name
    #[arg(long)]
    pub name: Option<String>,
    /// Filter by version
    #[arg(long)]
    pub version: Option<String>,
    /// Filter by OS
    #[arg(long)]
    pub os: Option<String>,
    /// Include public images
    #[arg(long)]
    pub public: bool,
    /// Filter by state
    #[arg(long)]
    pub state: Option<String>,
    /// Filter by type
    #[arg(long, name = "type")]
    pub image_type: Option<String>,

    /// Include all images (including inactive, disabled, etc.)
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Custom output fields (comma-separated)
    #[arg(short = 'o', long)]
    pub output: Option<String>,

    /// Long output format with more columns
    #[arg(short = 'l', long)]
    pub long: bool,

    /// Omit table header
    #[arg(short = 'H', long = "no-header")]
    pub no_header: bool,

    /// Show only short ID (one per line)
    #[arg(long)]
    pub short: bool,

    /// Sort by field (name, version, published_at, etc.)
    #[arg(short = 's', long)]
    pub sort_by: Option<String>,
}

#[derive(Args, Clone)]
pub struct ImageGetArgs {
    /// Image ID or name[@version]
    pub image: String,
}

#[derive(Args, Clone)]
pub struct ImageCreateArgs {
    /// Instance ID or name
    pub instance: String,
    /// Image name
    #[arg(long)]
    pub name: String,
    /// Image version
    #[arg(long)]
    pub version: Option<String>,
    /// Image description
    #[arg(long)]
    pub description: Option<String>,
    /// Image homepage URL
    #[arg(long)]
    pub homepage: Option<String>,
    /// Image EULA URL
    #[arg(long)]
    pub eula: Option<String>,
    /// Access control list (account UUIDs, multiple allowed)
    #[arg(long)]
    pub acl: Option<Vec<String>>,
    /// Tags (key=value, multiple allowed)
    #[arg(short = 't', long = "tag")]
    pub tags: Option<Vec<String>>,
    /// Wait for image to be active
    #[arg(long, short)]
    pub wait: bool,
    /// Dry run - show what would be created without creating
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Clone)]
pub struct ImageDeleteArgs {
    /// Image ID(s) or name[@version]
    pub images: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct ImageCloneArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Dry run - show what would be cloned without cloning
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Clone)]
pub struct ImageCopyArgs {
    /// Image ID or name[@version] in source datacenter
    pub image: String,
    /// Source datacenter name (positional or --source)
    #[arg(index = 2)]
    pub datacenter: Option<String>,
    /// Source datacenter name (alternative to positional)
    #[arg(long)]
    pub source: Option<String>,
    /// Dry run - show what would be copied without copying
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Clone)]
pub struct ImageUpdateArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// New name
    #[arg(long)]
    pub name: Option<String>,
    /// New version
    #[arg(long)]
    pub version: Option<String>,
    /// New description
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Args, Clone)]
pub struct ImageExportArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Manta path for export
    #[arg(long)]
    pub manta_path: String,
}

#[derive(Args, Clone)]
pub struct ImageWaitArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Target state (default: active)
    #[arg(short = 's', long, default_value = "active")]
    pub state: String,
    /// Timeout in seconds
    #[arg(long, default_value = "600")]
    pub timeout: u64,
}

#[derive(Args, Clone)]
pub struct ImageShareArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Account UUID to share the image with
    pub account: String,
}

#[derive(Args, Clone)]
pub struct ImageUnshareArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Account UUID to unshare the image from
    pub account: String,
}

#[derive(Args, Clone)]
pub struct ImageTagListArgs {
    /// Image ID or name[@version]
    pub image: String,
}

#[derive(Args, Clone)]
pub struct ImageTagGetArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Tag key
    pub key: String,
}

#[derive(Args, Clone)]
pub struct ImageTagSetArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Tags to set (key=value, multiple allowed)
    #[arg(required = true)]
    pub tags: Vec<String>,
}

#[derive(Args, Clone)]
pub struct ImageTagDeleteArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Tag key to delete
    pub key: String,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

impl ImageCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_images(args, client, use_json).await,
            Self::Get(args) => get_image(args, client, use_json).await,
            Self::Create(args) => create_image(args, client, use_json).await,
            Self::Delete(args) => delete_images(args, client).await,
            Self::Clone(args) => clone_image(args, client, use_json).await,
            Self::Copy(args) => copy_image(args, client, use_json).await,
            Self::Update(args) => update_image(args, client, use_json).await,
            Self::Export(args) => export_image(args, client, use_json).await,
            Self::Share(args) => share_image(args, client, use_json).await,
            Self::Unshare(args) => unshare_image(args, client, use_json).await,
            Self::Tag { command } => command.run(client, use_json).await,
            Self::Wait(args) => wait_image(args, client, use_json).await,
        }
    }
}

impl ImageTagCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_image_tags(args, client, use_json).await,
            Self::Get(args) => get_image_tag(args, client).await,
            Self::Set(args) => set_image_tags(args, client).await,
            Self::Delete(args) => delete_image_tag(args, client).await,
        }
    }
}

async fn list_images(args: ImageListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let mut req = client.inner().list_images().account(account);

    if let Some(name) = &args.name {
        req = req.name(name);
    }
    if let Some(version) = &args.version {
        req = req.version(version);
    }
    if let Some(os) = &args.os {
        req = req.os(os);
    }
    // If --all is not set and no explicit state filter, default to "active"
    if let Some(state) = &args.state {
        req = req.state(state);
    } else if !args.all {
        req = req.state("active");
    }
    if args.public {
        req = req.public(true);
    }

    let response = req.send().await?;
    let mut images = response.into_inner();

    // Sort images if requested
    if let Some(ref sort_field) = args.sort_by {
        sort_images(&mut images, sort_field);
    }

    if use_json {
        json::print_json(&images)?;
    } else {
        print_images_table(&images, &args);
    }

    Ok(())
}

fn sort_images(images: &mut [Image], field: &str) {
    match field.to_lowercase().as_str() {
        "name" => images.sort_by(|a, b| a.name.cmp(&b.name)),
        "version" => images.sort_by(|a, b| a.version.cmp(&b.version)),
        "os" => images.sort_by(|a, b| a.os.cmp(&b.os)),
        "type" => images.sort_by(|a, b| format!("{:?}", a.type_).cmp(&format!("{:?}", b.type_))),
        "state" => images.sort_by(|a, b| {
            let a_state = a
                .state
                .as_ref()
                .map(|s| format!("{:?}", s))
                .unwrap_or_default();
            let b_state = b
                .state
                .as_ref()
                .map(|s| format!("{:?}", s))
                .unwrap_or_default();
            a_state.cmp(&b_state)
        }),
        "published_at" | "published" => images.sort_by(|a, b| {
            let a_pub = a
                .published_at
                .as_ref()
                .map(|t| t.to_string())
                .unwrap_or_default();
            let b_pub = b
                .published_at
                .as_ref()
                .map(|t| t.to_string())
                .unwrap_or_default();
            a_pub.cmp(&b_pub)
        }),
        _ => {} // Unknown field, don't sort
    }
}

fn print_images_table(images: &[Image], args: &ImageListArgs) {
    // Handle --short: just print IDs
    if args.short {
        for img in images {
            let short_id = &img.id.to_string()[..8];
            println!("{}", short_id);
        }
        return;
    }

    // Determine columns based on --long or --output
    let columns: Vec<&str> = if let Some(ref output) = args.output {
        output.split(',').map(|s| s.trim()).collect()
    } else if args.long {
        vec![
            "id",
            "name",
            "version",
            "state",
            "type",
            "os",
            "public",
            "published",
        ]
    } else {
        vec!["shortid", "name", "version", "state", "type", "os"]
    };

    // Create header (uppercase)
    let headers: Vec<String> = columns.iter().map(|c| c.to_uppercase()).collect();
    let header_refs: Vec<&str> = headers.iter().map(|s| s.as_str()).collect();

    let mut tbl = if args.no_header {
        table::create_table_no_header(columns.len())
    } else {
        table::create_table(&header_refs)
    };

    for img in images {
        let row: Vec<String> = columns
            .iter()
            .map(|col| get_image_field_value(img, col))
            .collect();
        let row_refs: Vec<&str> = row.iter().map(|s| s.as_str()).collect();
        tbl.add_row(row_refs);
    }

    table::print_table(tbl);
}

/// Get a field value from an Image by field name
fn get_image_field_value(img: &Image, field: &str) -> String {
    match field.to_lowercase().as_str() {
        "id" => img.id.to_string(),
        "shortid" => img.id.to_string()[..8].to_string(),
        "name" => img.name.clone(),
        "version" => img.version.clone(),
        "state" => img
            .state
            .as_ref()
            .map(|s| format!("{:?}", s).to_lowercase())
            .unwrap_or_else(|| "-".to_string()),
        "type" => format!("{:?}", img.type_).to_lowercase(),
        "os" => img.os.clone(),
        "description" | "desc" => img.description.clone().unwrap_or_else(|| "-".to_string()),
        "public" => img
            .public
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string()),
        "owner" => img.owner.clone().unwrap_or_else(|| "-".to_string()),
        "published" | "published_at" => format_published(&img.published_at),
        "size" | "image_size" => img
            .image_size
            .map(format_size)
            .unwrap_or_else(|| "-".to_string()),
        "homepage" => img.homepage.clone().unwrap_or_else(|| "-".to_string()),
        "eula" => img.eula.clone().unwrap_or_else(|| "-".to_string()),
        "origin" => img.origin.clone().unwrap_or_else(|| "-".to_string()),
        _ => "-".to_string(),
    }
}

fn format_published(published_at: &Option<String>) -> String {
    match published_at {
        Some(timestamp) => {
            // Try to parse as RFC 3339
            if let Ok(dt) = DateTime::parse_from_rfc3339(timestamp) {
                let now = Utc::now();
                let dt_utc = dt.with_timezone(&Utc);
                let duration = now.signed_duration_since(dt_utc);

                if duration.num_days() >= 365 {
                    format!("{}y", duration.num_days() / 365)
                } else if duration.num_days() >= 30 {
                    format!("{}mo", duration.num_days() / 30)
                } else if duration.num_days() >= 1 {
                    format!("{}d", duration.num_days())
                } else if duration.num_hours() >= 1 {
                    format!("{}h", duration.num_hours())
                } else {
                    "now".to_string()
                }
            } else {
                // Fall back to raw timestamp if parsing fails
                timestamp.clone()
            }
        }
        None => "-".to_string(),
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{}B", bytes)
    }
}

async fn get_image(args: ImageGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;

    let response = client
        .inner()
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
        println!("Version:     {}", image.version);
        if let Some(state) = &image.state {
            println!("State:       {:?}", state);
        }
        println!("Type:        {:?}", image.type_);
        println!("OS:          {}", image.os);
        if let Some(desc) = &image.description {
            println!("Description: {}", desc);
        }
        println!("Public:      {}", image.public.unwrap_or(false));
    }

    Ok(())
}

async fn create_image(args: ImageCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let machine_id =
        crate::commands::instance::get::resolve_instance(&args.instance, client).await?;

    // ACL is just Vec<String> (account UUIDs as strings)
    let acl = args.acl.clone();

    // Parse tags into serde_json::Map
    let tags = if let Some(tag_strings) = &args.tags {
        let mut tag_map: Map<String, Value> = Map::new();
        for tag in tag_strings {
            if let Some((key, value)) = tag.split_once('=') {
                tag_map.insert(key.to_string(), Value::String(value.to_string()));
            } else {
                return Err(anyhow::anyhow!(
                    "Invalid tag format '{}', expected key=value",
                    tag
                ));
            }
        }
        Some(tag_map)
    } else {
        None
    };

    let request = cloudapi_client::types::CreateImageRequest {
        machine: machine_id.clone(),
        name: args.name.clone(),
        version: args.version.clone(),
        description: args.description.clone(),
        homepage: args.homepage.clone(),
        eula: args.eula.clone(),
        acl,
        tags,
    };

    // Handle dry-run
    if args.dry_run {
        println!("Dry run - would create image:");
        println!("  Name:        {}", args.name);
        if let Some(ver) = &args.version {
            println!("  Version:     {}", ver);
        }
        if let Some(desc) = &args.description {
            println!("  Description: {}", desc);
        }
        if let Some(hp) = &args.homepage {
            println!("  Homepage:    {}", hp);
        }
        if let Some(eula) = &args.eula {
            println!("  EULA:        {}", eula);
        }
        if let Some(acl) = &args.acl {
            println!("  ACL:         {:?}", acl);
        }
        if let Some(tags) = &args.tags {
            println!("  Tags:        {:?}", tags);
        }
        println!("  From instance: {}", args.instance);
        return Ok(());
    }

    let response = client
        .inner()
        .create_image_from_machine()
        .account(account)
        .body(request)
        .send()
        .await?;
    let image = response.into_inner();
    println!(
        "Creating image {} ({})",
        image.name,
        &image.id.to_string()[..8]
    );

    if args.wait {
        wait_for_image_state(&image.id.to_string(), "active", 600, client).await?;
        println!("Image is active");
    }

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn delete_images(args: ImageDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for image_name in &args.images {
        let image_id = resolve_image(image_name, client).await?;

        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete image '{}'?", image_name))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        client
            .inner()
            .delete_image()
            .account(account)
            .dataset(&image_id)
            .send()
            .await?;

        println!("Deleted image {}", image_name);
    }

    Ok(())
}

async fn clone_image(args: ImageCloneArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;
    let image_uuid: cloudapi_client::Uuid = image_id.parse()?;

    // Handle dry-run
    if args.dry_run {
        println!("Dry run - would clone image:");
        println!("  Source image: {} ({})", args.image, &image_id[..8]);
        return Ok(());
    }

    let image = client.clone_image(account, &image_uuid).await?;
    println!(
        "Cloned image {} ({})",
        image.name,
        &image.id.to_string()[..8]
    );

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn copy_image(args: ImageCopyArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // Get source datacenter from either positional arg or --source flag
    let source_dc = args.datacenter.or(args.source).ok_or_else(|| {
        anyhow::anyhow!("Source datacenter required (as second argument or --source)")
    })?;

    // For copy from another datacenter, we need to use import_image_from_datacenter
    // The image ID provided is from the source datacenter
    let source_image_uuid: cloudapi_client::Uuid = args
        .image
        .parse()
        .map_err(|_| anyhow::anyhow!("Image copy requires a UUID from the source datacenter"))?;

    // Handle dry-run
    if args.dry_run {
        println!("Dry run - would copy image:");
        println!("  Source image: {}", args.image);
        println!("  From datacenter: {}", source_dc);
        return Ok(());
    }

    // Create a placeholder UUID for the local image - the API will create a new one
    let local_uuid = source_image_uuid.clone();

    let image = client
        .import_image_from_datacenter(account, &local_uuid, source_dc.clone(), source_image_uuid)
        .await?;

    println!(
        "Copying image from {}: {} ({})",
        source_dc,
        image.name,
        &image.id.to_string()[..8]
    );

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn update_image(args: ImageUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;
    let image_uuid: cloudapi_client::Uuid = image_id.parse()?;

    let request = cloudapi_client::UpdateImageRequest {
        name: args.name.clone(),
        version: args.version.clone(),
        description: args.description.clone(),
        homepage: None,
        eula: None,
        acl: None,
        tags: None,
    };

    let image = client
        .update_image_metadata(account, &image_uuid, &request)
        .await?;
    println!("Updated image {}", image.name);

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn export_image(args: ImageExportArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;
    let image_uuid: cloudapi_client::Uuid = image_id.parse()?;

    let image = client
        .export_image(account, &image_uuid, args.manta_path.clone())
        .await?;

    println!("Exporting image {} to {}", image.name, args.manta_path);

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn wait_image(args: ImageWaitArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let image_id = resolve_image(&args.image, client).await?;

    wait_for_image_state(&image_id, &args.state, args.timeout, client).await?;
    println!("Image reached state: {}", args.state);

    // Get final image state
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(&image_id)
        .send()
        .await?;
    let image = response.into_inner();

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

/// Resolve image name[@version] or short ID to full UUID
pub async fn resolve_image(id_or_name: &str, client: &TypedClient) -> Result<String> {
    // UUID check
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    // Parse name@version
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

    // Try short ID match first (at least 8 characters)
    if id_or_name.len() >= 8 {
        for img in &images {
            if img.id.to_string().starts_with(id_or_name) {
                return Ok(img.id.to_string());
            }
        }
    }

    // Match by name (and optionally version)
    for img in &images {
        if let Some(v) = version {
            if img.version == v {
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
    client: &TypedClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = &client.auth_config().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let response = client
            .inner()
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

        if current_state == "failed" {
            return Err(anyhow::anyhow!("Image entered failed state"));
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for image state: {}",
                target_state
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn share_image(args: ImageShareArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;
    let image_uuid: cloudapi_client::Uuid = image_id.parse()?;
    let target_account: cloudapi_client::Uuid = args
        .account
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid account UUID: {}", args.account))?;

    let image = client
        .share_image(account, &image_uuid, target_account)
        .await?;

    println!("Shared image {} with account {}", image.name, args.account);

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn unshare_image(args: ImageUnshareArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;
    let image_uuid: cloudapi_client::Uuid = image_id.parse()?;
    let target_account: cloudapi_client::Uuid = args
        .account
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid account UUID: {}", args.account))?;

    let image = client
        .unshare_image(account, &image_uuid, target_account)
        .await?;

    println!(
        "Unshared image {} from account {}",
        image.name, args.account
    );

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

// =============================================================================
// Image Tag Functions
// =============================================================================

async fn list_image_tags(
    args: ImageTagListArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;

    // Get image to retrieve tags
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(&image_id)
        .send()
        .await?;

    let image = response.into_inner();
    let tags = image.tags.unwrap_or_default();

    if use_json {
        json::print_json(&tags)?;
    } else if tags.is_empty() {
        println!("No tags on image {}", image.name);
    } else {
        let mut tbl = table::create_table(&["KEY", "VALUE"]);
        for (key, value) in tags.iter() {
            let value_str = match value {
                serde_json::Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            tbl.add_row(vec![key, &value_str]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_image_tag(args: ImageTagGetArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;

    // Get image to retrieve tags
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(&image_id)
        .send()
        .await?;

    let image = response.into_inner();
    let tags = image.tags.unwrap_or_default();

    if let Some(value) = tags.get(&args.key) {
        let value_str = match value {
            serde_json::Value::String(s) => s.clone(),
            _ => value.to_string(),
        };
        println!("{}", value_str);
    } else {
        return Err(anyhow::anyhow!("Tag '{}' not found on image", args.key));
    }

    Ok(())
}

async fn set_image_tags(args: ImageTagSetArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;
    let image_uuid: cloudapi_client::Uuid = image_id.parse()?;

    // Get existing image to merge tags
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(&image_id)
        .send()
        .await?;

    let image = response.into_inner();
    // Convert Map to HashMap
    let mut tags: HashMap<String, Value> = image.tags.unwrap_or_default().into_iter().collect();

    // Parse and add new tags
    for tag in &args.tags {
        if let Some((key, value)) = tag.split_once('=') {
            tags.insert(key.to_string(), Value::String(value.to_string()));
        } else {
            return Err(anyhow::anyhow!(
                "Invalid tag format '{}', expected key=value",
                tag
            ));
        }
    }

    // Update image with new tags
    let request = cloudapi_client::UpdateImageRequest {
        name: None,
        version: None,
        description: None,
        homepage: None,
        eula: None,
        acl: None,
        tags: Some(tags.clone()),
    };

    client
        .update_image_metadata(account, &image_uuid, &request)
        .await?;

    for tag in &args.tags {
        if let Some((key, value)) = tag.split_once('=') {
            println!("Set tag {}={}", key, value);
        }
    }

    Ok(())
}

async fn delete_image_tag(args: ImageTagDeleteArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt(format!("Delete tag {}?", args.key))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let account = &client.auth_config().account;
    let image_id = resolve_image(&args.image, client).await?;
    let image_uuid: cloudapi_client::Uuid = image_id.parse()?;

    // Get existing image to remove tag
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(&image_id)
        .send()
        .await?;

    let image = response.into_inner();
    // Convert Map to HashMap
    let mut tags: HashMap<String, Value> = image.tags.unwrap_or_default().into_iter().collect();

    if tags.remove(&args.key).is_none() {
        return Err(anyhow::anyhow!("Tag '{}' not found on image", args.key));
    }

    // Update image with removed tag
    let request = cloudapi_client::UpdateImageRequest {
        name: None,
        version: None,
        description: None,
        homepage: None,
        eula: None,
        acl: None,
        tags: Some(tags),
    };

    client
        .update_image_metadata(account, &image_uuid, &request)
        .await?;

    println!("Deleted tag {}", args.key);

    Ok(())
}
