// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Image management commands for tritonadm (IMGAPI operations).

mod nocloud;

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use uuid::Uuid;

use imgapi_client::types;
use imgapi_client::{
    CreateImageFromVmRequest, CreateImageRequest, ExportImageResponse, ImportImageRequest,
    JobResponse, StorageType, UpdateImageRequest,
};

fn parse_uuid_list(s: &str) -> Result<Vec<Uuid>> {
    s.split(',')
        .map(|s| {
            s.trim()
                .parse::<Uuid>()
                .map_err(|e| anyhow::anyhow!("Invalid UUID '{}': {}", s.trim(), e))
        })
        .collect()
}

fn print_image_summary(image: &types::Image) {
    println!("UUID: {}", image.uuid);
    println!("Name: {} v{}", image.name, image.version);
    println!("State: {}", crate::enum_to_display(&image.state));
    if let Some(ref os) = image.os {
        println!("OS: {}", crate::enum_to_display(os));
    }
    if let Some(ref t) = image.type_ {
        println!("Type: {}", crate::enum_to_display(t));
    }
    println!("Owner: {}", image.owner);
    println!("Public: {}", image.public);
    if let Some(ref published) = image.published_at {
        println!("Published: {}", published);
    }
    if !image.files.is_empty() {
        println!("Files: {}", image.files.len());
    }
}

/// Print a summary for an API-crate Image (returned by TypedClient)
fn print_api_image(image: &imgapi_client::Image) {
    println!("UUID: {}", image.uuid);
    println!("Name: {} v{}", image.name, image.version);
    println!("State: {}", crate::enum_to_display(&image.state));
    if let Some(ref os) = image.os {
        println!("OS: {}", crate::enum_to_display(os));
    }
    if let Some(ref t) = image.image_type {
        println!("Type: {}", crate::enum_to_display(t));
    }
    println!("Owner: {}", image.owner);
    println!("Public: {}", image.public);
    if let Some(ref published) = image.published_at {
        println!("Published: {}", published);
    }
    if !image.files.is_empty() {
        println!("Files: {}", image.files.len());
    }
}

fn print_job_response(resp: &JobResponse) {
    println!("Image UUID: {}", resp.image_uuid);
    println!("Job UUID: {}", resp.job_uuid);
}

fn print_export_response(resp: &ExportImageResponse) {
    println!("Manta URL: {}", resp.manta_url);
    println!("Image path: {}", resp.image_path);
    println!("Manifest path: {}", resp.manifest_path);
}

/// Import a manifest+file pair into IMGAPI. Used by both
/// `tritonadm image import` and `tritonadm image fetch-nocloud --target imgapi`.
///
/// `compression` is the explicit override from the CLI; if `None`,
/// the manifest's `files[0].compression` field is used as a fallback.
pub(super) async fn import_manifest_and_file(
    client: &imgapi_client::Client,
    typed_client: &imgapi_client::TypedClient,
    manifest: &str,
    file: &str,
    compression: Option<types::FileCompression>,
    updates_url: Option<&str>,
) -> Result<()> {
    let manifest_str = tokio::fs::read_to_string(manifest)
        .await
        .map_err(|e| anyhow::anyhow!("failed to read manifest '{}': {}", manifest, e))?;
    let manifest_value: serde_json::Value = serde_json::from_str(&manifest_str)
        .map_err(|e| anyhow::anyhow!("invalid manifest JSON: {}", e))?;

    let uuid_str = manifest_value
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("manifest missing 'uuid' field"))?;
    let uuid: Uuid = uuid_str
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid UUID in manifest: {}", e))?;

    let name = manifest_value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let version = manifest_value
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let manifest_compression = manifest_value
        .get("files")
        .and_then(|f| f.as_array())
        .and_then(|a| a.first())
        .and_then(|f| f.get("compression"))
        .and_then(|c| c.as_str())
        .map(String::from);

    let origin_uuid = manifest_value
        .get("origin")
        .and_then(|v| v.as_str())
        .map(|s| {
            s.parse::<Uuid>()
                .map_err(|e| anyhow::anyhow!("invalid origin UUID: {}", e))
        })
        .transpose()?;
    let source = updates_url.unwrap_or(crate::DEFAULT_UPDATES_URL);
    super::imgapi_util::ensure_origin_imported(client, typed_client, origin_uuid, source, None)
        .await?;

    eprintln!("Importing image manifest {uuid}...");
    client
        .image_action()
        .uuid(uuid)
        .action(imgapi_client::types::ImageAction::Import)
        .body(manifest_value)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to import manifest: {}", e))?;
    eprintln!("Imported: {name} v{version}");

    eprintln!("Uploading image file...");
    let file_bytes = tokio::fs::read(file)
        .await
        .map_err(|e| anyhow::anyhow!("failed to read file '{}': {}", file, e))?;

    let mut req = client.add_image_file().uuid(uuid).body(file_bytes);
    if let Some(c) = compression {
        req = req.compression(c);
    } else if let Some(ref c) = manifest_compression {
        req = req.compression(c.as_str());
    }
    req.send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to upload image file: {}", e))?;
    eprintln!("Image file uploaded.");

    eprintln!("Activating image...");
    typed_client
        .activate_image(&uuid)
        .await
        .map_err(|e| anyhow::anyhow!("failed to activate image: {}", e))?;
    eprintln!("Image {uuid} imported and activated.");
    Ok(())
}

#[derive(Subcommand)]
pub enum ImageCommand {
    // ========================================================================
    // Convenience
    // ========================================================================
    /// Import an image from manifest + file (like sdc-imgadm import -m -f)
    Import {
        /// Path to image manifest JSON file
        #[arg(short = 'm', long)]
        manifest: String,
        /// Path to image file (ZFS dataset, etc.)
        #[arg(short = 'f', long)]
        file: String,
        /// File compression type (default: auto-detect from manifest)
        #[arg(short = 'c', long, value_enum)]
        compression: Option<types::FileCompression>,
    },

    /// Fetch a CloudInit nocloud image from an upstream vendor and convert
    /// it into a SmartOS/Triton zvol image + IMGAPI manifest.
    #[command(name = "fetch-nocloud")]
    FetchNocloud {
        /// Upstream vendor profile. Mutually exclusive with --vendor-toml.
        #[arg(long, value_enum, required_unless_present = "vendor_toml")]
        vendor: Option<nocloud::Vendor>,
        /// Vendor-specific release token (e.g. "noble", "jammy", "latest").
        /// Required with --vendor; ignored with --vendor-toml.
        #[arg(long, required_unless_present = "vendor_toml")]
        release: Option<String>,
        /// Path to a TOML profile that pins a fully-resolved
        /// (url, sha256, metadata) tuple. See
        /// docs/design/examples/nocloud-vendors/ for the schema and
        /// worked examples. Mutually exclusive with --vendor.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["vendor", "release"])]
        vendor_toml: Option<PathBuf>,
        /// Where to deliver the produced manifest + image:
        ///   `file`     leave artifacts in --output-dir (default);
        ///   `smartos`  shell `imgadm install` against the local SmartOS
        ///              image store (GZ-only);
        ///   `imgapi`   push to IMGAPI via the existing import path.
        #[arg(long, value_enum, default_value_t)]
        target: nocloud::Target,
        /// Output dir for *.zfs.gz and *.json
        /// (default: /var/tmp/tritonadm/nocloud/image/<vendor>-<series>)
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// Working dir for downloads / intermediates
        /// (default: /var/tmp/tritonadm/nocloud/cache/<vendor>-<series>)
        #[arg(long)]
        workdir: Option<PathBuf>,
        /// Override delegated dataset
        /// (default: zones/<zonename>/data, or `zones` in the GZ)
        #[arg(long)]
        dataset: Option<String>,
        /// Skip checksum/signature verification (DANGEROUS, dev only)
        #[arg(long)]
        insecure_no_verify: bool,
        /// Override the vendor's verifier with a pinned sha256 (hex).
        /// Useful for vendors that don't publish a per-image hash
        /// (e.g. Talos), or for pinning to a known-good build.
        #[arg(long, value_name = "HEX")]
        expected_sha256: Option<String>,
        /// Resolve metadata and print the build plan, but don't download,
        /// hash, write any files, or touch any datasets.
        #[arg(long)]
        dry_run: bool,
    },

    // ========================================================================
    // Ping / State
    // ========================================================================
    /// Health check endpoint
    Ping {
        /// Trigger a specific error for testing
        #[arg(long)]
        error: Option<String>,
    },

    /// Get admin state snapshot (admin-only)
    #[command(name = "admin-get-state")]
    AdminGetState,

    /// Drop internal caches (admin-only)
    #[command(name = "admin-drop-caches")]
    AdminDropCaches,

    // ========================================================================
    // Channels
    // ========================================================================
    /// List channels
    #[command(name = "list-channels")]
    ListChannels {
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Images - CRUD
    // ========================================================================
    /// List images
    #[command(name = "list-images", alias = "list")]
    ListImages {
        /// Filter by owner UUID
        #[arg(long)]
        owner: Option<Uuid>,
        /// Filter by name
        #[arg(long)]
        name: Option<String>,
        /// Filter by version
        #[arg(long)]
        version: Option<String>,
        /// Filter by state
        #[arg(long, value_enum)]
        state: Option<types::ImageState>,
        /// Filter by type
        #[arg(long, value_enum, name = "type")]
        image_type: Option<types::ImageType>,
        /// Filter by OS
        #[arg(long, value_enum)]
        os: Option<types::ImageOs>,
        /// Filter public images only
        #[arg(long)]
        public: Option<bool>,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
        /// Pagination limit
        #[arg(long)]
        limit: Option<u64>,
        /// Pagination marker (UUID of last image in previous page)
        #[arg(long)]
        marker: Option<Uuid>,
        /// Filter by tag (key=value)
        #[arg(long)]
        tag: Option<String>,
        /// Filter by billing tag
        #[arg(long)]
        billing_tag: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get image details
    #[command(name = "get-image")]
    GetImage {
        /// Image UUID
        uuid: Uuid,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create image from manifest
    #[command(name = "create-image")]
    CreateImage {
        /// Image manifest as JSON string
        #[arg(long)]
        manifest_json: String,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    /// Create image from an existing VM
    #[command(name = "create-image-from-vm")]
    CreateImageFromVm {
        /// VM UUID
        #[arg(long)]
        vm_uuid: Uuid,
        /// Image name
        #[arg(long)]
        name: String,
        /// Image version
        #[arg(long)]
        version: String,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Whether to make incremental
        #[arg(long)]
        incremental: Option<bool>,
        /// Maximum origin chain depth
        #[arg(long)]
        max_origin_depth: Option<u32>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
    },

    /// Delete an image
    #[command(name = "delete-image", alias = "delete")]
    DeleteImage {
        /// Image UUID
        uuid: Uuid,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
        /// Force delete across all channels
        #[arg(long)]
        force_all_channels: bool,
    },

    // ========================================================================
    // Image Actions
    // ========================================================================
    /// Activate an unactivated image
    #[command(name = "activate-image")]
    ActivateImage {
        /// Image UUID
        uuid: Uuid,
    },

    /// Enable a disabled image
    #[command(name = "enable-image")]
    EnableImage {
        /// Image UUID
        uuid: Uuid,
    },

    /// Disable an active image
    #[command(name = "disable-image")]
    DisableImage {
        /// Image UUID
        uuid: Uuid,
    },

    /// Update mutable image fields
    #[command(name = "update-image")]
    UpdateImage {
        /// Image UUID
        uuid: Uuid,
        /// Updated name
        #[arg(long)]
        name: Option<String>,
        /// Updated version
        #[arg(long)]
        version: Option<String>,
        /// Updated description
        #[arg(long)]
        description: Option<String>,
        /// Updated homepage URL
        #[arg(long)]
        homepage: Option<String>,
        /// Updated EULA URL
        #[arg(long)]
        eula: Option<String>,
        /// Updated public flag
        #[arg(long)]
        public: Option<bool>,
        /// Updated tags (JSON)
        #[arg(long)]
        tags_json: Option<String>,
    },

    /// Export image to Manta
    #[command(name = "export-image")]
    ExportImage {
        /// Image UUID
        uuid: Uuid,
        /// Manta path
        #[arg(long)]
        manta_path: String,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
    },

    /// Add image to a channel
    #[command(name = "channel-add-image")]
    ChannelAddImage {
        /// Image UUID
        uuid: Uuid,
        /// Channel name
        #[arg(long)]
        channel: String,
    },

    /// Change storage backend (admin-only)
    #[command(name = "change-stor")]
    ChangeStor {
        /// Image UUID
        uuid: Uuid,
        /// Target storage backend
        #[arg(long, value_enum)]
        stor: types::StorageType,
    },

    /// Import an image manifest (admin-only)
    #[command(name = "import-image")]
    ImportImage {
        /// Image UUID
        uuid: Uuid,
        /// Import manifest as JSON string
        #[arg(long)]
        manifest_json: String,
    },

    /// Import from remote IMGAPI (admin-only, creates job)
    #[command(name = "import-remote-image")]
    ImportRemoteImage {
        /// Image UUID
        uuid: Uuid,
        /// Source IMGAPI URL
        #[arg(long)]
        source: String,
        /// Skip owner check
        #[arg(long)]
        skip_owner_check: bool,
    },

    /// Import from another datacenter (creates job)
    #[command(name = "import-from-datacenter")]
    ImportFromDatacenter {
        /// Image UUID
        uuid: Uuid,
        /// Datacenter name
        #[arg(long)]
        datacenter: String,
        /// Account UUID
        #[arg(long)]
        account: Uuid,
    },

    // ========================================================================
    // File Management
    // ========================================================================
    /// Upload image file from local path
    #[command(name = "add-image-file")]
    AddImageFile {
        /// Image UUID
        uuid: Uuid,
        /// Path to file to upload
        #[arg(long)]
        file: String,
        /// File compression type
        #[arg(long, value_enum)]
        compression: Option<types::FileCompression>,
        /// SHA-1 hash for verification
        #[arg(long)]
        sha1: Option<String>,
        /// File size for verification
        #[arg(long)]
        size: Option<u64>,
        /// Source IMGAPI URL (for remote copy)
        #[arg(long)]
        source: Option<String>,
        /// Storage backend
        #[arg(long, value_enum)]
        storage: Option<types::StorageType>,
        /// Dataset GUID
        #[arg(long)]
        dataset_guid: Option<String>,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    /// Add image file from URL
    #[command(name = "add-image-file-from-url")]
    AddImageFileFromUrl {
        /// Image UUID
        uuid: Uuid,
        /// URL to fetch the image file from
        #[arg(long)]
        file_url: String,
        /// Compression type
        #[arg(long, value_enum)]
        compression: Option<types::FileCompression>,
        /// SHA-1 hash for verification
        #[arg(long)]
        sha1: Option<String>,
        /// File size for verification
        #[arg(long)]
        size: Option<u64>,
        /// Storage backend
        #[arg(long, value_enum)]
        storage: Option<types::StorageType>,
        /// Dataset GUID
        #[arg(long)]
        dataset_guid: Option<String>,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    /// Download image file
    #[command(name = "get-image-file")]
    GetImageFile {
        /// Image UUID
        uuid: Uuid,
        /// File index (default: 0)
        #[arg(long)]
        index: Option<u32>,
        /// Output file path (default: stdout)
        #[arg(long, short)]
        output: Option<String>,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    // ========================================================================
    // Icon Management
    // ========================================================================
    /// Upload image icon
    #[command(name = "add-image-icon")]
    AddImageIcon {
        /// Image UUID
        uuid: Uuid,
        /// Path to icon file to upload
        #[arg(long)]
        file: String,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    /// Download image icon
    #[command(name = "get-image-icon")]
    GetImageIcon {
        /// Image UUID
        uuid: Uuid,
        /// Output file path (default: stdout)
        #[arg(long, short)]
        output: Option<String>,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    /// Delete image icon
    #[command(name = "delete-image-icon")]
    DeleteImageIcon {
        /// Image UUID
        uuid: Uuid,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    // ========================================================================
    // ACL Management
    // ========================================================================
    /// Add account UUIDs to image ACL
    #[command(name = "add-image-acl")]
    AddImageAcl {
        /// Image UUID
        uuid: Uuid,
        /// Account UUIDs to add (comma-separated)
        #[arg(long)]
        acl_uuids: String,
    },

    /// Remove account UUIDs from image ACL
    #[command(name = "remove-image-acl")]
    RemoveImageAcl {
        /// Image UUID
        uuid: Uuid,
        /// Account UUIDs to remove (comma-separated)
        #[arg(long)]
        acl_uuids: String,
    },

    // ========================================================================
    // Jobs
    // ========================================================================
    /// List jobs for an image
    #[command(name = "list-image-jobs")]
    ListImageJobs {
        /// Image UUID
        uuid: Uuid,
        /// Filter by task name
        #[arg(long)]
        task: Option<String>,
        /// Account UUID
        #[arg(long)]
        account: Option<Uuid>,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Clone
    // ========================================================================
    /// Clone an image (dc mode only)
    #[command(name = "clone-image")]
    CloneImage {
        /// Image UUID
        uuid: Uuid,
        /// Account UUID (required)
        #[arg(long)]
        account: Uuid,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    // ========================================================================
    // Admin Push
    // ========================================================================
    /// Push a Docker image (admin-only, streaming)
    #[command(name = "admin-push-image")]
    AdminPushImage {
        /// Image UUID
        uuid: Uuid,
        /// Channel name
        #[arg(long)]
        channel: Option<String>,
    },

    // ========================================================================
    // Auth Keys
    // ========================================================================
    /// Reload auth keys (admin-only)
    #[command(name = "admin-reload-auth-keys")]
    AdminReloadAuthKeys,

    // ========================================================================
    // Legacy Datasets
    // ========================================================================
    /// List datasets (legacy redirect to /images)
    #[command(name = "list-datasets")]
    ListDatasets,

    /// Get dataset (legacy redirect)
    #[command(name = "get-dataset")]
    GetDataset {
        /// Dataset argument (URN or UUID)
        arg: String,
    },
}

impl ImageCommand {
    pub async fn run(self, imgapi_url: Result<String>, updates_url: Option<&str>) -> Result<()> {
        // Dispatch fetch-nocloud before resolving IMGAPI: it has no use
        // for IMGAPI and must work in builder zones with no headnode.
        if let ImageCommand::FetchNocloud {
            vendor,
            release,
            vendor_toml,
            target,
            output_dir,
            workdir,
            dataset,
            insecure_no_verify,
            expected_sha256,
            dry_run,
        } = self
        {
            return nocloud::run(nocloud::FetchOpts {
                vendor,
                release,
                vendor_toml,
                output_dir,
                workdir,
                insecure_no_verify,
                expected_sha256,
                dataset,
                dry_run,
                target,
                imgapi_url,
                updates_url: updates_url.map(str::to_string),
            })
            .await;
        }

        let imgapi_url = imgapi_url?;
        let http = triton_tls::build_http_client(false)
            .await
            .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {}", e))?;
        let client = imgapi_client::Client::new_with_client(&imgapi_url, http.clone());
        let typed_client = imgapi_client::TypedClient::new_with_client(&imgapi_url, http);

        match self {
            // ================================================================
            // Convenience
            // ================================================================
            ImageCommand::FetchNocloud { .. } => {
                unreachable!("fetch-nocloud dispatched before IMGAPI client setup")
            }

            ImageCommand::Import {
                manifest,
                file,
                compression,
            } => {
                import_manifest_and_file(
                    &client,
                    &typed_client,
                    &manifest,
                    &file,
                    compression,
                    updates_url,
                )
                .await?;
            }

            // ================================================================
            // Ping / State
            // ================================================================
            ImageCommand::Ping { error } => {
                let mut req = client.ping();
                if let Some(ref e) = error {
                    req = req.error(e);
                }
                let resp = req.send().await?;
                println!("{}", serde_json::to_string_pretty(&resp.into_inner())?);
            }

            ImageCommand::AdminGetState => {
                let resp = client.admin_get_state().send().await?;
                println!("{}", serde_json::to_string_pretty(&resp.into_inner())?);
            }

            ImageCommand::AdminDropCaches => {
                typed_client
                    .drop_caches()
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Caches dropped (202 Accepted)");
            }

            // ================================================================
            // Channels
            // ================================================================
            ImageCommand::ListChannels { raw } => {
                let resp = client.list_channels().send().await?;
                let channels = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&channels)?);
                } else {
                    for ch in &channels {
                        let default_str = if ch.default.unwrap_or(false) {
                            " (default)"
                        } else {
                            ""
                        };
                        println!("{}: {}{}", ch.name, ch.description, default_str);
                    }
                    println!("\nTotal: {} channels", channels.len());
                }
            }

            // ================================================================
            // Images - CRUD
            // ================================================================
            ImageCommand::ListImages {
                owner,
                name,
                version,
                state,
                image_type,
                os,
                public,
                account,
                channel,
                limit,
                marker,
                tag,
                billing_tag,
                raw,
            } => {
                let mut req = client.list_images();
                if let Some(v) = owner {
                    req = req.owner(v);
                }
                if let Some(ref v) = name {
                    req = req.name(v);
                }
                if let Some(ref v) = version {
                    req = req.version(v);
                }
                if let Some(v) = state {
                    req = req.state(v);
                }
                if let Some(v) = image_type {
                    req = req.type_(v);
                }
                if let Some(v) = os {
                    req = req.os(v);
                }
                if let Some(v) = public {
                    req = req.public(v);
                }
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                if let Some(v) = limit {
                    req = req.limit(v);
                }
                if let Some(v) = marker {
                    req = req.marker(v);
                }
                if let Some(ref v) = tag {
                    req = req.tag(v);
                }
                if let Some(ref v) = billing_tag {
                    req = req.billing_tag(v);
                }
                let resp = req.send().await?;
                let images = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&images)?);
                } else {
                    // Match sdc-imgadm list format
                    println!(
                        "{:<38} {:<32} {:<45} {:<6} {:<8} PUBLISHED",
                        "UUID", "NAME", "VERSION", "FLAGS", "OS"
                    );
                    for img in &images {
                        let mut flags = String::new();
                        if !img.public {
                            flags.push('P');
                        }
                        if img.state == types::ImageState::Active {
                            // no flag for active
                        } else if img.state == types::ImageState::Disabled {
                            flags.push('D');
                        } else if img.state == types::ImageState::Unactivated {
                            flags.push('I');
                        } else {
                            flags.push('X');
                        }
                        if flags.is_empty() {
                            flags.push('-');
                        }
                        let os = img
                            .os
                            .as_ref()
                            .map(crate::enum_to_display)
                            .unwrap_or_else(|| "-".to_string());
                        let published = img.published_at.as_deref().unwrap_or("-");
                        println!(
                            "{:<38} {:<32} {:<45} {:<6} {:<8} {}",
                            img.uuid, img.name, img.version, flags, os, published,
                        );
                    }
                }
            }

            ImageCommand::GetImage {
                uuid,
                account,
                channel,
                raw,
            } => {
                let mut req = client.get_image().uuid(uuid);
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let image = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&image)?);
                } else {
                    print_image_summary(&image);
                }
            }

            ImageCommand::CreateImage {
                manifest_json,
                channel,
            } => {
                let manifest: CreateImageRequest = serde_json::from_str(&manifest_json)
                    .map_err(|e| anyhow::anyhow!("Invalid manifest JSON: {}", e))?;
                let image = typed_client
                    .create_image_from_manifest(&manifest, channel.as_deref())
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image created:");
                print_api_image(&image);
            }

            ImageCommand::CreateImageFromVm {
                vm_uuid,
                name,
                version,
                description,
                incremental,
                max_origin_depth,
                channel,
                account,
            } => {
                let request = CreateImageFromVmRequest {
                    vm_uuid,
                    name,
                    version,
                    description,
                    homepage: None,
                    eula: None,
                    acl: None,
                    tags: None,
                    incremental,
                    max_origin_depth,
                    os: None,
                    image_type: None,
                };
                let resp: JobResponse = typed_client
                    .create_image_from_vm(&request, channel.as_deref(), account.as_ref())
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image creation from VM initiated:");
                print_job_response(&resp);
            }

            ImageCommand::DeleteImage {
                uuid,
                account,
                channel,
                force_all_channels,
            } => {
                let mut req = client.delete_image().uuid(uuid);
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                if force_all_channels {
                    req = req.force_all_channels(true);
                }
                req.send().await?;
                println!("Image {} deleted", uuid);
            }

            // ================================================================
            // Image Actions
            // ================================================================
            ImageCommand::ActivateImage { uuid } => {
                let image = typed_client
                    .activate_image(&uuid)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image activated:");
                print_api_image(&image);
            }

            ImageCommand::EnableImage { uuid } => {
                let image = typed_client
                    .enable_image(&uuid)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image enabled:");
                print_api_image(&image);
            }

            ImageCommand::DisableImage { uuid } => {
                let image = typed_client
                    .disable_image(&uuid)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image disabled:");
                print_api_image(&image);
            }

            ImageCommand::UpdateImage {
                uuid,
                name,
                version,
                description,
                homepage,
                eula,
                public,
                tags_json,
            } => {
                let tags = match tags_json {
                    Some(ref s) => Some(
                        serde_json::from_str(s)
                            .map_err(|e| anyhow::anyhow!("Invalid tags JSON: {}", e))?,
                    ),
                    None => None,
                };
                let request = UpdateImageRequest {
                    name,
                    version,
                    description,
                    homepage,
                    eula,
                    public,
                    tags,
                    acl: None,
                    requirements: None,
                    users: None,
                    generate_passwords: None,
                    inherited_directories: None,
                    billing_tags: None,
                    traits: None,
                    state: None,
                    error: None,
                };
                let image = typed_client
                    .update_image(&uuid, &request)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image updated:");
                print_api_image(&image);
            }

            ImageCommand::ExportImage {
                uuid,
                manta_path,
                account,
            } => {
                let resp: ExportImageResponse = typed_client
                    .export_image(&uuid, &manta_path, account.as_ref())
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image exported:");
                print_export_response(&resp);
            }

            ImageCommand::ChannelAddImage { uuid, channel } => {
                let image = typed_client
                    .channel_add_image(&uuid, &channel)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image added to channel '{}':", channel);
                print_api_image(&image);
            }

            ImageCommand::ChangeStor { uuid, stor } => {
                let api_stor = match stor {
                    types::StorageType::Local => StorageType::Local,
                    types::StorageType::Manta => StorageType::Manta,
                    _ => {
                        return Err(anyhow::anyhow!("Unsupported storage type"));
                    }
                };
                let image = typed_client
                    .change_stor(&uuid, api_stor)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Storage changed:");
                print_api_image(&image);
            }

            ImageCommand::ImportImage {
                uuid,
                manifest_json,
            } => {
                let manifest: ImportImageRequest = serde_json::from_str(&manifest_json)
                    .map_err(|e| anyhow::anyhow!("Invalid manifest JSON: {}", e))?;
                let image = typed_client
                    .import_image(&uuid, &manifest)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Image imported:");
                print_api_image(&image);
            }

            ImageCommand::ImportRemoteImage {
                uuid,
                source,
                skip_owner_check,
            } => {
                let resp: JobResponse = typed_client
                    .import_remote_image(&uuid, &source, skip_owner_check)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Remote import initiated:");
                print_job_response(&resp);
            }

            ImageCommand::ImportFromDatacenter {
                uuid,
                datacenter,
                account,
            } => {
                let resp: JobResponse = typed_client
                    .import_from_datacenter(&uuid, &datacenter, &account)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("Datacenter import initiated:");
                print_job_response(&resp);
            }

            // ================================================================
            // File Management
            // ================================================================
            ImageCommand::AddImageFile {
                uuid,
                file,
                compression,
                sha1,
                size,
                source,
                storage,
                dataset_guid,
                account,
                channel,
            } => {
                let file_bytes = tokio::fs::read(&file)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to read file '{}': {}", file, e))?;
                let mut req = client.add_image_file().uuid(uuid).body(file_bytes);
                if let Some(v) = compression {
                    req = req.compression(v);
                }
                if let Some(ref v) = sha1 {
                    req = req.sha1(v);
                }
                if let Some(v) = size {
                    req = req.size(v);
                }
                if let Some(ref v) = source {
                    req = req.source(v);
                }
                if let Some(v) = storage {
                    req = req.storage(v);
                }
                if let Some(ref v) = dataset_guid {
                    req = req.dataset_guid(v);
                }
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let image = resp.into_inner();
                println!("File added to image:");
                print_image_summary(&image);
            }

            ImageCommand::AddImageFileFromUrl {
                uuid,
                file_url,
                compression,
                sha1,
                size,
                storage,
                dataset_guid,
                account,
                channel,
            } => {
                let mut body_builder = types::AddImageFileFromUrlRequest::builder();
                body_builder = body_builder.file_url(file_url);
                if let Some(v) = compression {
                    body_builder = body_builder.compression(v);
                }
                if let Some(v) = sha1 {
                    body_builder = body_builder.sha1(v);
                }
                if let Some(v) = size {
                    body_builder = body_builder.size(v);
                }
                if let Some(v) = storage {
                    body_builder = body_builder.storage(v);
                }
                if let Some(v) = dataset_guid {
                    body_builder = body_builder.dataset_guid(v);
                }
                let body: types::AddImageFileFromUrlRequest =
                    body_builder
                        .try_into()
                        .map_err(|e: types::error::ConversionError| {
                            anyhow::anyhow!("Failed to build request: {}", e)
                        })?;
                let mut req = client.add_image_file_from_url().uuid(uuid).body(body);
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let image = resp.into_inner();
                println!("File from URL added to image:");
                print_image_summary(&image);
            }

            ImageCommand::GetImageFile {
                uuid,
                index,
                output,
                account,
                channel,
            } => {
                let mut req = client.get_image_file().uuid(uuid);
                if let Some(v) = index {
                    req = req.index(v);
                }
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let stream = resp.into_inner();
                use futures_util::TryStreamExt;
                let chunks: Vec<bytes::Bytes> = stream.into_inner().try_collect().await?;
                let mut data = Vec::new();
                for chunk in chunks {
                    data.extend_from_slice(&chunk);
                }
                match output {
                    Some(path) => {
                        tokio::fs::write(&path, &data)
                            .await
                            .map_err(|e| anyhow::anyhow!("Failed to write to '{}': {}", path, e))?;
                        println!("Image file written to {} ({} bytes)", path, data.len());
                    }
                    None => {
                        use std::io::Write;
                        std::io::stdout()
                            .write_all(&data)
                            .map_err(|e| anyhow::anyhow!("Failed to write to stdout: {}", e))?;
                    }
                }
            }

            // ================================================================
            // Icon Management
            // ================================================================
            ImageCommand::AddImageIcon {
                uuid,
                file,
                account,
                channel,
            } => {
                let file_bytes = tokio::fs::read(&file)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to read file '{}': {}", file, e))?;
                let mut req = client.add_image_icon().uuid(uuid).body(file_bytes);
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let image = resp.into_inner();
                println!("Icon added to image:");
                print_image_summary(&image);
            }

            ImageCommand::GetImageIcon {
                uuid,
                output,
                account,
                channel,
            } => {
                let mut req = client.get_image_icon().uuid(uuid);
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let stream = resp.into_inner();
                use futures_util::TryStreamExt;
                let chunks: Vec<bytes::Bytes> = stream.into_inner().try_collect().await?;
                let mut data = Vec::new();
                for chunk in chunks {
                    data.extend_from_slice(&chunk);
                }
                match output {
                    Some(path) => {
                        tokio::fs::write(&path, &data)
                            .await
                            .map_err(|e| anyhow::anyhow!("Failed to write to '{}': {}", path, e))?;
                        println!("Icon written to {} ({} bytes)", path, data.len());
                    }
                    None => {
                        use std::io::Write;
                        std::io::stdout()
                            .write_all(&data)
                            .map_err(|e| anyhow::anyhow!("Failed to write to stdout: {}", e))?;
                    }
                }
            }

            ImageCommand::DeleteImageIcon {
                uuid,
                account,
                channel,
            } => {
                let mut req = client.delete_image_icon().uuid(uuid);
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let image = resp.into_inner();
                println!("Icon deleted:");
                print_image_summary(&image);
            }

            // ================================================================
            // ACL Management
            // ================================================================
            ImageCommand::AddImageAcl { uuid, acl_uuids } => {
                let uuids = parse_uuid_list(&acl_uuids)?;
                let image = typed_client
                    .add_image_acl(&uuid, &uuids)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("ACL updated (added):");
                print_image_summary(&image);
            }

            ImageCommand::RemoveImageAcl { uuid, acl_uuids } => {
                let uuids = parse_uuid_list(&acl_uuids)?;
                let image = typed_client
                    .remove_image_acl(&uuid, &uuids)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                println!("ACL updated (removed):");
                print_image_summary(&image);
            }

            // ================================================================
            // Jobs
            // ================================================================
            ImageCommand::ListImageJobs {
                uuid,
                task,
                account,
                channel,
                raw,
            } => {
                let mut req = client.list_image_jobs().uuid(uuid);
                if let Some(ref v) = task {
                    req = req.task(v);
                }
                if let Some(v) = account {
                    req = req.account(v);
                }
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let jobs = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&jobs)?);
                } else {
                    for job in &jobs {
                        println!("{}", serde_json::to_string_pretty(job)?);
                    }
                    println!("\nTotal: {} jobs", jobs.len());
                }
            }

            // ================================================================
            // Clone
            // ================================================================
            ImageCommand::CloneImage {
                uuid,
                account,
                channel,
            } => {
                let mut req = client.clone_image().uuid(uuid).account(account);
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let image = resp.into_inner();
                println!("Image cloned:");
                print_image_summary(&image);
            }

            // ================================================================
            // Admin Push
            // ================================================================
            ImageCommand::AdminPushImage { uuid, channel } => {
                let mut req = client.admin_push_image().uuid(uuid);
                if let Some(ref v) = channel {
                    req = req.channel(v);
                }
                let resp = req.send().await?;
                let stream = resp.into_inner();
                // Streaming response - print chunks as they arrive
                use futures_util::TryStreamExt;
                let chunks: Vec<bytes::Bytes> = stream.into_inner().try_collect().await?;
                for chunk in &chunks {
                    let text = String::from_utf8_lossy(chunk);
                    print!("{}", text);
                }
            }

            // ================================================================
            // Auth Keys
            // ================================================================
            ImageCommand::AdminReloadAuthKeys => {
                let resp = client.admin_reload_auth_keys().send().await?;
                println!("{}", serde_json::to_string_pretty(&resp.into_inner())?);
            }

            // ================================================================
            // Legacy Datasets
            // ================================================================
            ImageCommand::ListDatasets => {
                let resp = client.list_datasets().send().await?;
                let stream = resp.into_inner();
                use futures_util::TryStreamExt;
                let chunks: Vec<bytes::Bytes> = stream.into_inner().try_collect().await?;
                for chunk in &chunks {
                    let text = String::from_utf8_lossy(chunk);
                    print!("{}", text);
                }
            }

            ImageCommand::GetDataset { arg } => {
                let resp = client.get_dataset().arg(&arg).send().await?;
                let stream = resp.into_inner();
                use futures_util::TryStreamExt;
                let chunks: Vec<bytes::Bytes> = stream.into_inner().try_collect().await?;
                for chunk in &chunks {
                    let text = String::from_utf8_lossy(chunk);
                    print!("{}", text);
                }
            }
        }

        Ok(())
    }
}
