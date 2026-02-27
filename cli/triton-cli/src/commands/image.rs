// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image management commands

use std::collections::HashMap;

use anyhow::Result;
use chrono::DateTime;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use cloudapi_client::types::{Image, ImageState};
use dialoguer::Confirm;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::define_columns;
use crate::output::table::{TableBuilder, TableFormatArgs};
use crate::output::{enum_to_display, json, opt_enum_to_display, table};

#[derive(Subcommand, Clone)]
pub enum ImageCommand {
    /// List images
    #[command(visible_alias = "ls")]
    List(ImageListArgs),
    /// Get image details
    Get(ImageGetArgs),
    /// Create image from instance
    Create(ImageCreateArgs),
    /// Delete image
    #[command(visible_alias = "rm")]
    Delete(ImageDeleteArgs),
    /// Clone image to account
    Clone(ImageCloneArgs),
    /// Copy image from another datacenter
    #[command(visible_alias = "cp")]
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
    #[command(visible_alias = "ls")]
    List(ImageTagListArgs),

    /// Get a tag value
    Get(ImageTagGetArgs),

    /// Set tag(s) on an image
    Set(ImageTagSetArgs),

    /// Delete a tag from an image
    #[command(visible_alias = "rm")]
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
    #[arg(long, value_enum)]
    pub state: Option<cloudapi_client::types::ImageState>,
    /// Filter by type
    #[arg(long, name = "type", value_enum)]
    pub image_type: Option<cloudapi_client::types::ImageType>,

    /// Include all images (including inactive, disabled, etc.)
    #[arg(short = 'a', long)]
    pub all: bool,

    #[command(flatten)]
    pub table: TableFormatArgs,

    /// Show only short ID (one per line)
    #[arg(long)]
    pub short: bool,

    /// Filters in key=value format (e.g., name=base-64, state=active, type=zone-dataset)
    ///
    /// Supported filter keys: name, os, version, public, state, owner, type
    #[arg(trailing_var_arg = true)]
    pub filters: Vec<String>,
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
    pub name: String,
    /// Image version
    pub version: Option<String>,
    /// Image description
    #[arg(long, short = 'd')]
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
    /// Image ID or name[@version]
    pub image: String,
    /// Destination datacenter name
    pub datacenter: String,
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
    /// New homepage URL
    #[arg(long)]
    pub homepage: Option<String>,
    /// New EULA URL
    #[arg(long)]
    pub eula: Option<String>,
    /// field=value pairs (e.g. name=new-name version=2.0.0)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub fields: Vec<String>,
}

#[derive(Args, Clone)]
pub struct ImageExportArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Manta path for export
    pub manta_path: String,
    /// Dry run - show what would be exported without exporting
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Clone)]
pub struct ImageWaitArgs {
    /// Image ID(s) or name[@version]
    #[arg(required = true)]
    pub images: Vec<String>,
    /// Target state(s) to wait for (comma-separated or multiple -s flags)
    /// Default is "active,failed"
    #[arg(short = 's', long = "states", value_enum, value_delimiter = ',', default_values_t = vec![cloudapi_client::types::ImageState::Active, cloudapi_client::types::ImageState::Failed])]
    pub states: Vec<cloudapi_client::types::ImageState>,
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
    /// Dry run - show what would be shared without sharing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Clone)]
pub struct ImageUnshareArgs {
    /// Image ID or name[@version]
    pub image: String,
    /// Account UUID to unshare the image from
    pub account: String,
    /// Dry run - show what would be unshared without unsharing
    #[arg(long)]
    pub dry_run: bool,
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
    pub async fn run(
        self,
        client: &TypedClient,
        use_json: bool,
        cache: Option<&crate::cache::ImageCache>,
    ) -> Result<()> {
        match self {
            Self::List(args) => list_images(args, client, use_json, cache).await,
            Self::Get(args) => get_image(args, client, use_json, cache).await,
            Self::Create(args) => create_image(args, client, use_json).await,
            Self::Delete(args) => delete_images(args, client, cache).await,
            Self::Clone(args) => clone_image(args, client, use_json, cache).await,
            Self::Copy(args) => copy_image(args, client, use_json, cache).await,
            Self::Update(args) => update_image(args, client, use_json, cache).await,
            Self::Export(args) => export_image(args, client, use_json, cache).await,
            Self::Share(args) => share_image(args, client, use_json, cache).await,
            Self::Unshare(args) => unshare_image(args, client, use_json, cache).await,
            Self::Tag { command } => command.run(client, use_json, cache).await,
            Self::Wait(args) => wait_image(args, client, use_json, cache).await,
        }
    }
}

impl ImageTagCommand {
    pub async fn run(
        self,
        client: &TypedClient,
        use_json: bool,
        cache: Option<&crate::cache::ImageCache>,
    ) -> Result<()> {
        match self {
            Self::List(args) => list_image_tags(args, client, use_json, cache).await,
            Self::Get(args) => get_image_tag(args, client, cache).await,
            Self::Set(args) => set_image_tags(args, client, cache).await,
            Self::Delete(args) => delete_image_tag(args, client, cache).await,
        }
    }
}

/// Valid filter keys for positional key=value arguments
const VALID_FILTERS: &[&str] = &["name", "os", "version", "public", "state", "owner", "type"];

/// Check if a filter key is valid
fn is_valid_filter(key: &str) -> bool {
    VALID_FILTERS.contains(&key)
}

/// Deserialize a serde enum from its wire-format string value.
fn parse_serde_enum<T: DeserializeOwned>(value: &str) -> std::result::Result<T, serde_json::Error> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
}

/// Apply positional key=value filters to the ImageListArgs, merging with any
/// existing --flag values. Positional filters override flags if both are set.
/// Returns an optional owner UUID filter (from `owner=` positional arg).
fn apply_positional_filters(args: &mut ImageListArgs) -> Result<Option<cloudapi_client::Uuid>> {
    let mut owner = None;
    for filter in std::mem::take(&mut args.filters) {
        let (key, value) = filter
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid filter '{}': must be key=value", filter))?;

        if !is_valid_filter(key) {
            anyhow::bail!(
                "Unknown filter '{}'. Valid filters: {}",
                key,
                VALID_FILTERS.join(", ")
            );
        }

        match key {
            "name" => args.name = Some(value.to_string()),
            "os" => args.os = Some(value.to_string()),
            "version" => args.version = Some(value.to_string()),
            "public" => {
                args.public = value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid public value '{}': expected true or false", value)
                })?;
            }
            "state" => {
                args.state = Some(parse_serde_enum(value).map_err(|_| {
                    anyhow::anyhow!(
                        "Invalid state value '{}': expected active, disabled, etc.",
                        value
                    )
                })?);
            }
            "owner" => {
                owner = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid owner value '{}': expected UUID", value)
                })?);
            }
            "type" => {
                args.image_type = Some(parse_serde_enum(value).map_err(|_| {
                    anyhow::anyhow!(
                        "Invalid type value '{}': expected zone-dataset, lx-dataset, zvol, etc.",
                        value
                    )
                })?);
            }
            _ => unreachable!(),
        }
    }
    Ok(owner)
}

async fn list_images(
    mut args: ImageListArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let owner = apply_positional_filters(&mut args)?;
    let account = client.effective_account();

    // Determine if this is an unfiltered query that can use/populate cache
    let is_unfiltered = args.name.is_none()
        && args.version.is_none()
        && args.os.is_none()
        && args.image_type.is_none()
        && !args.public
        && owner.is_none();
    let is_default_state = args.state.is_none() && !args.all;

    // Try cache for unfiltered default queries
    let images = if is_unfiltered && is_default_state {
        match cache {
            Some(c) => match c.load_list().await {
                Some(cached) => cached,
                None => {
                    let response = client.inner().list_images().account(account).send().await?;
                    let fetched = response.into_inner();
                    c.save_list(&fetched).await;
                    fetched
                }
            },
            None => {
                let response = client.inner().list_images().account(account).send().await?;
                response.into_inner()
            }
        }
    } else {
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
        if let Some(state) = args.state {
            req = req.state(state);
        }
        if args.public {
            req = req.public(true);
        }
        if let Some(image_type) = args.image_type {
            req = req.type_(image_type);
        }
        if let Some(owner) = owner {
            req = req.owner(owner);
        }
        let response = req.send().await?;
        response.into_inner()
    };

    // Filter to active-only for default state queries since the cache may
    // contain all states (populated by instance list's state=All fetch).
    let images = if is_default_state {
        images
            .into_iter()
            .filter(|img| {
                img.state
                    .as_ref()
                    .is_none_or(|s| matches!(s, ImageState::Active))
            })
            .collect()
    } else {
        images
    };

    if use_json {
        json::print_json_stream(&images)?;
    } else {
        print_images_table(&images, &args)?;
    }

    Ok(())
}

define_columns! {
    ImageColumn for Image {
        ShortId("SHORTID") => |img| img.id.to_string()[..8].to_string(),
        Name("NAME") => |img| img.name.clone(),
        Version("VERSION") => |img| img.version.clone(),
        Flags("FLAGS") => |img| {
            let mut flags = String::new();
            if img.origin.is_some() {
                flags.push('I');
            }
            let is_public = img.public.unwrap_or(false);
            let has_acl = img.acl.as_ref().map(|a| !a.is_empty()).unwrap_or(false);
            if has_acl && !is_public {
                flags.push('S');
            }
            if is_public {
                flags.push('P');
            }
            if flags.is_empty() { "-".to_string() } else { flags }
        },
        Os("OS") => |img| img.os.clone(),
        Type("TYPE") => |img| enum_to_display(&img.type_),
        PubDate("PUBDATE") => |img| format_pubdate(&img.published_at),
        State("STATE") => |img| img.state.as_ref().map(enum_to_display)
            .unwrap_or_else(|| "-".to_string()),
        Id("ID") => |img| img.id.to_string(),
        Public("PUBLIC") => |img| img.public.map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string()),
    }
}

fn print_images_table(images: &[Image], args: &ImageListArgs) -> Result<()> {
    // Handle --short: just print IDs
    if args.short {
        for img in images {
            let short_id = &img.id.to_string()[..8];
            println!("{}", short_id);
        }
        return Ok(());
    }

    // Set default columns based on short/long mode to match node-triton.
    // All columns remain available for -o selection (long_from: None).
    let mut table_opts = args.table.clone();
    if table_opts.columns.is_none() {
        table_opts.columns = Some(
            if table_opts.long {
                vec![
                    "ID", "NAME", "VERSION", "STATE", "FLAGS", "OS", "TYPE", "PUBDATE",
                ]
            } else {
                vec![
                    "SHORTID", "NAME", "VERSION", "FLAGS", "OS", "TYPE", "PUBDATE",
                ]
            }
            .into_iter()
            .map(String::from)
            .collect(),
        );
    }

    TableBuilder::from_enum_columns::<ImageColumn, _>(images, None).print(&table_opts)?;
    Ok(())
}

/// Format pubdate as YYYY-MM-DD (matching node-triton)
fn format_pubdate(published_at: &Option<String>) -> String {
    match published_at {
        Some(timestamp) => {
            // Try to parse as RFC 3339 and extract just the date part
            if let Ok(dt) = DateTime::parse_from_rfc3339(timestamp) {
                dt.format("%Y-%m-%d").to_string()
            } else {
                // Fall back to returning the raw string truncated to date
                timestamp.split('T').next().unwrap_or("-").to_string()
            }
        }
        None => "-".to_string(),
    }
}

async fn get_image(
    args: ImageGetArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(image_uuid)
        .send()
        .await?;

    let image = response.into_inner();

    if use_json {
        json::print_json(&image)?;
    } else {
        json::print_json_pretty(&image)?;
    }

    Ok(())
}

async fn create_image(args: ImageCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let machine_id =
        crate::commands::instance::get::resolve_instance(&args.instance, client).await?;

    // ACL is Vec<Uuid> (account UUIDs)
    let acl = if let Some(acl_strings) = &args.acl {
        let mut acl_uuids = Vec::new();
        for acl_str in acl_strings {
            let uuid: uuid::Uuid = acl_str
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid ACL UUID: {}", acl_str))?;
            acl_uuids.push(uuid);
        }
        Some(acl_uuids)
    } else {
        None
    };

    // Parse tags into HashMap (matches the Tags type alias)
    let tags = if let Some(tag_strings) = &args.tags {
        let mut tag_map = HashMap::new();
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

    let request = cloudapi_client::CreateImageRequest {
        machine: machine_id,
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
            println!("  ACL:         {}", acl.join(", "));
        }
        if let Some(tags) = &args.tags {
            println!(
                "  Tags:        {}",
                tags.iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("  From instance: {}", args.instance);
        return Ok(());
    }

    let response = client
        .inner()
        .create_or_import_image()
        .account(account)
        .body(serde_json::to_value(&request)?)
        .send()
        .await?;
    let image = response.into_inner();
    println!(
        "Creating image {} ({})",
        image.name,
        &image.id.to_string()[..8]
    );

    if args.wait {
        wait_for_image_states(
            image.id,
            &[ImageState::Active, ImageState::Failed],
            600,
            client,
        )
        .await?;
        println!("Image is active");
    }

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn delete_images(
    args: ImageDeleteArgs,
    client: &TypedClient,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();

    for image_name in &args.images {
        let image_uuid = resolve_image_no_verify(image_name, client, cache).await?;

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
            .dataset(image_uuid)
            .send()
            .await?;

        println!("Deleted image {}", image_name);
    }

    Ok(())
}

async fn clone_image(
    args: ImageCloneArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    // Handle dry-run
    if args.dry_run {
        println!("Dry run - would clone image:");
        println!(
            "  Source image: {} ({})",
            args.image,
            &image_uuid.to_string()[..8]
        );
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

async fn copy_image(
    args: ImageCopyArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let dest_dc = &args.datacenter;

    // Resolve the image (supports name@version, shortID, UUID)
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    // List datacenters to find destination URL and our own DC name
    let datacenters = client
        .inner()
        .list_datacenters()
        .account(account)
        .send()
        .await?
        .into_inner();

    // Validate the destination datacenter exists
    let dest_url = datacenters.get(dest_dc).ok_or_else(|| {
        anyhow::anyhow!(
            "'{}' is not a valid datacenter name (available: {})",
            dest_dc,
            datacenters.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;

    // Determine our current DC name by reverse-looking up the base URL
    let my_url = client.baseurl().trim_end_matches('/');
    let source_dc = datacenters
        .iter()
        .find(|(_, url)| url.trim_end_matches('/') == my_url)
        .map(|(name, _)| name.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine current datacenter name from URL {}",
                my_url
            )
        })?;

    // Handle dry-run
    if args.dry_run {
        println!("Dry run - would copy image:");
        println!("  Image: {} ({})", args.image, &image_uuid.to_string()[..8]);
        println!("  From datacenter: {}", source_dc);
        println!("  To datacenter: {}", dest_dc);
        return Ok(());
    }

    // Create a client for the destination datacenter
    let dest_client = TypedClient::new_with_http_client(
        dest_url,
        client.auth_config().clone(),
        client.http_client().clone(),
    );

    let image = dest_client
        .import_image_from_datacenter(account, &source_dc, image_uuid)
        .await?;

    println!(
        "Copied image {}@{} ({}) to datacenter {}",
        image.name,
        image.version,
        &image.id.to_string()[..8],
        dest_dc
    );

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn update_image(
    args: ImageUpdateArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    // Start with --flag values
    let mut name = args.name.clone();
    let mut version = args.version.clone();
    let mut description = args.description.clone();
    let mut homepage = args.homepage.clone();
    let mut eula = args.eula.clone();
    let mut updated_fields = Vec::new();

    // Parse positional field=value pairs (flags take precedence)
    for field_arg in &args.fields {
        let (key, value) = field_arg
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid field=value pair: {field_arg}"))?;
        match key {
            "name" => {
                if name.is_none() {
                    name = Some(value.to_string());
                }
                updated_fields.push("name");
            }
            "version" => {
                if version.is_none() {
                    version = Some(value.to_string());
                }
                updated_fields.push("version");
            }
            "description" => {
                if description.is_none() {
                    description = Some(value.to_string());
                }
                updated_fields.push("description");
            }
            "homepage" => {
                if homepage.is_none() {
                    homepage = Some(value.to_string());
                }
                updated_fields.push("homepage");
            }
            "eula" => {
                if eula.is_none() {
                    eula = Some(value.to_string());
                }
                updated_fields.push("eula");
            }
            _ => anyhow::bail!("unknown field: {key}"),
        }
    }

    // Also track fields set via --flags
    if args.name.is_some() && !updated_fields.contains(&"name") {
        updated_fields.push("name");
    }
    if args.version.is_some() && !updated_fields.contains(&"version") {
        updated_fields.push("version");
    }
    if args.description.is_some() && !updated_fields.contains(&"description") {
        updated_fields.push("description");
    }
    if args.homepage.is_some() && !updated_fields.contains(&"homepage") {
        updated_fields.push("homepage");
    }
    if args.eula.is_some() && !updated_fields.contains(&"eula") {
        updated_fields.push("eula");
    }

    let request = cloudapi_client::UpdateImageRequest {
        name,
        version,
        description,
        homepage,
        eula,
        acl: None,
        tags: None,
    };

    let image = client
        .update_image_metadata(account, &image_uuid, &request)
        .await?;

    if updated_fields.is_empty() {
        println!("Updated image {}", image.name);
    } else {
        println!(
            "Updated image {} (fields: {})",
            image.name,
            updated_fields.join(", ")
        );
    }

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn export_image(
    args: ImageExportArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    // Handle dry-run
    if args.dry_run {
        println!("Dry run - would export image:");
        println!("  Image: {} ({})", args.image, &image_uuid.to_string()[..8]);
        println!("  Manta path: {}", args.manta_path);
        return Ok(());
    }

    let image = client
        .export_image(account, &image_uuid, args.manta_path.clone())
        .await?;

    println!("Exporting image {} to {}", image.name, args.manta_path);

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn wait_image(
    args: ImageWaitArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let total = args.images.len();
    let state_names: Vec<String> = args.states.iter().map(enum_to_display).collect();

    // First resolve all images and check if any are already in target state
    let mut images_to_wait: Vec<(uuid::Uuid, String)> = Vec::new(); // (id, display_name)
    let mut done = 0;

    for image_ref in &args.images {
        let image_uuid = resolve_image(image_ref, client, cache).await?;

        // Get current state
        let response = client
            .inner()
            .get_image()
            .account(account)
            .dataset(image_uuid)
            .send()
            .await?;
        let image = response.into_inner();

        if args.states.iter().any(|s| image.state.as_ref() == Some(s)) {
            done += 1;
            let current_state = opt_enum_to_display(image.state.as_ref());
            println!(
                "{}/{}: Image {} ({}@{}) already {}",
                done, total, image.id, image.name, image.version, current_state
            );
        } else {
            images_to_wait.push((
                image_uuid,
                format!("{} ({}@{})", image.id, image.name, image.version),
            ));
        }
    }

    if images_to_wait.is_empty() {
        return Ok(());
    }

    // Print waiting message
    if images_to_wait.len() == 1 {
        println!(
            "Waiting for image {} to enter state (states: {})",
            images_to_wait[0].1,
            state_names.join(", ")
        );
    } else {
        println!(
            "Waiting for {} images to enter state (states: {})",
            images_to_wait.len(),
            state_names.join(", ")
        );
    }

    // Wait for each image
    let mut final_images = Vec::new();
    for (image_uuid, display_name) in &images_to_wait {
        let image = wait_for_image_states(*image_uuid, &args.states, args.timeout, client).await?;
        done += 1;
        let final_state = opt_enum_to_display(image.state.as_ref());
        println!(
            "{}/{}: Image {} moved to state {}",
            done, total, display_name, final_state
        );
        final_images.push(image);
    }

    if use_json {
        json::print_json(&final_images)?;
    }

    Ok(())
}

/// Resolve image name[@version] or short ID to full UUID
///
/// Matches node-triton behavior (lib/tritonapi.js getImage):
/// - If full UUID, verify it exists with a GET then use directly
/// - Otherwise, list all images and match by name or short ID
/// - Short ID is the first segment of UUID (before first dash)
pub async fn resolve_image(
    id_or_name: &str,
    client: &TypedClient,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<uuid::Uuid> {
    resolve_image_inner(id_or_name, client, cache, true).await
}

/// Resolve image without verification GET for UUID inputs.
/// Used by delete paths where node-triton skips the verification.
async fn resolve_image_no_verify(
    id_or_name: &str,
    client: &TypedClient,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<uuid::Uuid> {
    resolve_image_inner(id_or_name, client, cache, false).await
}

async fn resolve_image_inner(
    id_or_name: &str,
    client: &TypedClient,
    cache: Option<&crate::cache::ImageCache>,
    verify_uuid: bool,
) -> Result<uuid::Uuid> {
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        if verify_uuid {
            // Verify the image exists (matches node-triton's getImage call)
            // In emit-payload mode, the exec hook returns a fake response
            let account = client.effective_account();
            client
                .inner()
                .get_image()
                .account(account)
                .dataset(uuid.to_string())
                .send()
                .await?;
        }
        return Ok(uuid);
    }

    // Parse name@version
    let (name, version) = if let Some(idx) = id_or_name.rfind('@') {
        (&id_or_name[..idx], Some(&id_or_name[idx + 1..]))
    } else {
        (id_or_name, None)
    };

    let account = client.effective_account();

    // For unfiltered lookups (no version), try cache first
    let images = if let Some(v) = version {
        // Version-filtered request — always go to API
        let response = client
            .inner()
            .list_images()
            .account(account)
            .name(name)
            .version(v)
            .send()
            .await?;
        response.into_inner()
    } else {
        let cached = match cache {
            Some(c) => c.load_list().await,
            None => None,
        };
        if let Some(cached) = cached {
            cached
        } else {
            let response = client.inner().list_images().account(account).send().await?;
            let fetched = response.into_inner();
            if let Some(c) = cache {
                c.save_list(&fetched).await;
            }
            fetched
        }
    };

    resolve_from_list(&images, name)
}

fn resolve_from_list(images: &[Image], name: &str) -> Result<uuid::Uuid> {
    let mut name_matches: Vec<_> = images.iter().filter(|img| img.name == name).collect();
    let short_id_matches: Vec<_> = images
        .iter()
        .filter(|img| img.id.to_string().starts_with(name))
        .collect();

    // Prefer name matches (sorted by published_at, return most recent)
    if !name_matches.is_empty() {
        name_matches.sort_by(|a, b| a.published_at.cmp(&b.published_at));
        if let Some(most_recent) = name_matches.last() {
            return Ok(most_recent.id);
        }
    }

    // Fall back to short ID matches
    if short_id_matches.len() == 1 {
        return Ok(short_id_matches[0].id);
    } else if short_id_matches.len() > 1 {
        return Err(crate::errors::ResourceNotFoundError(format!(
            "no image with name \"{}\" was found and \"{}\" is an ambiguous short id",
            name, name
        ))
        .into());
    }

    Err(crate::errors::ResourceNotFoundError(format!(
        "no image with name or short id \"{}\" was found",
        name
    ))
    .into())
}

async fn wait_for_image_states(
    image_uuid: uuid::Uuid,
    target_states: &[ImageState],
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<Image> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let response = client
            .inner()
            .get_image()
            .account(account)
            .dataset(image_uuid)
            .send()
            .await?;

        let image = response.into_inner();

        // Check if current state matches any target state
        if target_states
            .iter()
            .any(|s| image.state.as_ref() == Some(s))
        {
            return Ok(image);
        }

        if start.elapsed() > timeout {
            let target_names: Vec<String> = target_states.iter().map(enum_to_display).collect();
            return Err(anyhow::anyhow!(
                "Timeout waiting for image states: {}",
                target_names.join(", ")
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn share_image(
    args: ImageShareArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image_no_verify(&args.image, client, cache).await?;
    let target_account: cloudapi_client::Uuid = args
        .account
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid account UUID: {}", args.account))?;

    // Handle dry-run
    if args.dry_run {
        println!("Dry run - would share image:");
        println!("  Image: {} ({})", args.image, &image_uuid.to_string()[..8]);
        println!("  With account: {}", args.account);
        return Ok(());
    }

    let image = client
        .share_image(account, &image_uuid, target_account)
        .await?;

    println!("Shared image {} with account {}", image.name, args.account);

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn unshare_image(
    args: ImageUnshareArgs,
    client: &TypedClient,
    use_json: bool,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image_no_verify(&args.image, client, cache).await?;
    let target_account: cloudapi_client::Uuid = args
        .account
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid account UUID: {}", args.account))?;

    // Handle dry-run
    if args.dry_run {
        println!("Dry run - would unshare image:");
        println!("  Image: {} ({})", args.image, &image_uuid.to_string()[..8]);
        println!("  From account: {}", args.account);
        return Ok(());
    }

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
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    // Get image to retrieve tags
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(image_uuid)
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

async fn get_image_tag(
    args: ImageTagGetArgs,
    client: &TypedClient,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    // Get image to retrieve tags
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(image_uuid)
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
        return Err(crate::errors::ResourceNotFoundError(format!(
            "Tag '{}' not found on image",
            args.key
        ))
        .into());
    }

    Ok(())
}

async fn set_image_tags(
    args: ImageTagSetArgs,
    client: &TypedClient,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    let account = client.effective_account();
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    // Get existing image to merge tags
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(image_uuid)
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

async fn delete_image_tag(
    args: ImageTagDeleteArgs,
    client: &TypedClient,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt(format!("Delete tag {}?", args.key))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let account = client.effective_account();
    let image_uuid = resolve_image(&args.image, client, cache).await?;

    // Get existing image to remove tag
    let response = client
        .inner()
        .get_image()
        .account(account)
        .dataset(image_uuid)
        .send()
        .await?;

    let image = response.into_inner();
    // Convert Map to HashMap
    let mut tags: HashMap<String, Value> = image.tags.unwrap_or_default().into_iter().collect();

    if tags.remove(&args.key).is_none() {
        return Err(crate::errors::ResourceNotFoundError(format!(
            "Tag '{}' not found on image",
            args.key
        ))
        .into());
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
