// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Image management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

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
    /// Wait for image state
    Wait(ImageWaitArgs),
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
    /// Wait for image to be active
    #[arg(long, short)]
    pub wait: bool,
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
}

#[derive(Args, Clone)]
pub struct ImageCopyArgs {
    /// Image ID or name[@version] in source datacenter
    pub image: String,
    /// Source datacenter name
    #[arg(long)]
    pub source: String,
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
    #[arg(long, default_value = "active")]
    pub state: String,
    /// Timeout in seconds
    #[arg(long, default_value = "600")]
    pub timeout: u64,
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
            Self::Wait(args) => wait_image(args, client, use_json).await,
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
            let state_str = img
                .state
                .as_ref()
                .map(|s| format!("{:?}", s).to_lowercase())
                .unwrap_or_else(|| "-".to_string());
            tbl.add_row(vec![
                &img.id.to_string()[..8],
                &img.name,
                &img.version,
                &state_str,
                &format!("{:?}", img.type_).to_lowercase(),
                &img.os,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
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
    let machine_id = crate::commands::instance::get::resolve_instance(&args.instance, client).await?;
    let machine_uuid: cloudapi_client::Uuid = machine_id.parse()?;

    let request = cloudapi_client::types::CreateImageRequest {
        machine: machine_uuid,
        name: args.name.clone(),
        version: args.version.clone(),
        description: args.description.clone(),
        homepage: None,
        eula: None,
        acl: None,
        tags: None,
    };

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

    let image = client.clone_image(account, &image_uuid).await?;
    println!("Cloned image {} ({})", image.name, &image.id.to_string()[..8]);

    if use_json {
        json::print_json(&image)?;
    }

    Ok(())
}

async fn copy_image(args: ImageCopyArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // For copy from another datacenter, we need to use import_image_from_datacenter
    // The image ID provided is from the source datacenter
    let source_image_uuid: cloudapi_client::Uuid = args.image.parse()
        .map_err(|_| anyhow::anyhow!("Image copy requires a UUID from the source datacenter"))?;

    // Create a placeholder UUID for the local image - the API will create a new one
    let local_uuid = source_image_uuid.clone();

    let image = client
        .import_image_from_datacenter(account, &local_uuid, args.source.clone(), source_image_uuid)
        .await?;

    println!(
        "Copying image from source datacenter: {} ({})",
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

    let image = client.update_image_metadata(account, &image_uuid, &request).await?;
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
