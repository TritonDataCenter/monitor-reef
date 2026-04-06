// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SAPI CLI - Command-line interface for Triton SAPI
//!
//! This CLI provides access to all SAPI endpoints for managing applications,
//! services, instances, and manifests in Triton's Services and Configuration API.
//!
//! # Environment Variables
//!
//! - `SAPI_URL` - SAPI base URL (default: http://localhost)

use anyhow::Result;
use clap::{Parser, Subcommand};
use sapi_client::Client;
use sapi_client::types;
use uuid::Uuid;

/// Convert a serde-serializable enum value to its wire-format string.
fn enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", val))
}

#[derive(Parser)]
#[command(name = "sapi", version, about = "CLI for Triton SAPI")]
struct Cli {
    /// SAPI base URL
    #[arg(long, env = "SAPI_URL", default_value = "http://localhost")]
    base_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // ========================================================================
    // Ping
    // ========================================================================
    /// Health check endpoint
    Ping {
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Mode
    // ========================================================================
    /// Get current SAPI mode
    #[command(name = "get-mode")]
    GetMode {
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Set SAPI mode (only "full" is accepted)
    #[command(name = "set-mode")]
    SetMode {
        /// Mode to set
        mode: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Log Level
    // ========================================================================
    /// Get current log level
    #[command(name = "get-log-level")]
    GetLogLevel {
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Set log level
    #[command(name = "set-log-level")]
    SetLogLevel {
        /// Log level to set
        level: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Cache
    // ========================================================================
    /// Sync the SAPI cache
    #[command(name = "sync-cache")]
    SyncCache,

    // ========================================================================
    // Applications
    // ========================================================================
    /// List applications
    #[command(name = "list-applications")]
    ListApplications {
        /// Filter by name
        #[arg(long)]
        name: Option<String>,
        /// Filter by owner UUID
        #[arg(long)]
        owner_uuid: Option<Uuid>,
        /// Include master records from remote datacenter
        #[arg(long)]
        include_master: bool,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get an application by UUID
    #[command(name = "get-application")]
    GetApplication {
        /// Application UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a new application
    #[command(name = "create-application")]
    CreateApplication {
        /// Application name (required)
        #[arg(long)]
        name: String,
        /// Owner UUID (required)
        #[arg(long)]
        owner_uuid: Uuid,
        /// JSON params object
        #[arg(long)]
        params: Option<String>,
        /// JSON metadata object
        #[arg(long)]
        metadata: Option<String>,
        /// JSON metadata_schema object
        #[arg(long)]
        metadata_schema: Option<String>,
        /// JSON manifests object (name -> UUID mapping)
        #[arg(long)]
        manifests: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update an application
    #[command(name = "update-application")]
    UpdateApplication {
        /// Application UUID
        uuid: Uuid,
        /// Action: update (default), replace, or delete
        #[arg(long)]
        action: Option<String>,
        /// JSON params object
        #[arg(long)]
        params: Option<String>,
        /// JSON metadata object
        #[arg(long)]
        metadata: Option<String>,
        /// JSON metadata_schema object
        #[arg(long)]
        metadata_schema: Option<String>,
        /// JSON manifests object
        #[arg(long)]
        manifests: Option<String>,
        /// New owner UUID
        #[arg(long)]
        owner_uuid: Option<Uuid>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete an application
    #[command(name = "delete-application")]
    DeleteApplication {
        /// Application UUID
        uuid: Uuid,
    },

    // ========================================================================
    // Services
    // ========================================================================
    /// List services
    #[command(name = "list-services")]
    ListServices {
        /// Filter by name
        #[arg(long)]
        name: Option<String>,
        /// Filter by application UUID
        #[arg(long)]
        application_uuid: Option<Uuid>,
        /// Filter by type (vm or agent)
        #[arg(long)]
        service_type: Option<String>,
        /// Include master records from remote datacenter
        #[arg(long)]
        include_master: bool,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get a service by UUID
    #[command(name = "get-service")]
    GetService {
        /// Service UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a new service
    #[command(name = "create-service")]
    CreateService {
        /// Service name (required)
        #[arg(long)]
        name: String,
        /// Parent application UUID (required)
        #[arg(long)]
        application_uuid: Uuid,
        /// Service type (vm or agent)
        #[arg(long)]
        service_type: Option<String>,
        /// JSON params object
        #[arg(long)]
        params: Option<String>,
        /// JSON metadata object
        #[arg(long)]
        metadata: Option<String>,
        /// JSON manifests object
        #[arg(long)]
        manifests: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update a service
    #[command(name = "update-service")]
    UpdateService {
        /// Service UUID
        uuid: Uuid,
        /// Action: update (default), replace, or delete
        #[arg(long)]
        action: Option<String>,
        /// JSON params object
        #[arg(long)]
        params: Option<String>,
        /// JSON metadata object
        #[arg(long)]
        metadata: Option<String>,
        /// JSON manifests object
        #[arg(long)]
        manifests: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete a service
    #[command(name = "delete-service")]
    DeleteService {
        /// Service UUID
        uuid: Uuid,
    },

    // ========================================================================
    // Instances
    // ========================================================================
    /// List instances
    #[command(name = "list-instances")]
    ListInstances {
        /// Filter by service UUID
        #[arg(long)]
        service_uuid: Option<Uuid>,
        /// Filter by type (vm or agent)
        #[arg(long)]
        instance_type: Option<String>,
        /// Include master records from remote datacenter
        #[arg(long)]
        include_master: bool,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get an instance by UUID
    #[command(name = "get-instance")]
    GetInstance {
        /// Instance UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a new instance
    #[command(name = "create-instance")]
    CreateInstance {
        /// Parent service UUID (required)
        #[arg(long)]
        service_uuid: Uuid,
        /// Instance UUID (optional, auto-generated if not provided)
        #[arg(long)]
        uuid: Option<Uuid>,
        /// Run asynchronously (return job UUID instead of waiting)
        #[arg(long)]
        async_create: bool,
        /// JSON params object
        #[arg(long)]
        params: Option<String>,
        /// JSON metadata object
        #[arg(long)]
        metadata: Option<String>,
        /// JSON manifests object
        #[arg(long)]
        manifests: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update an instance
    #[command(name = "update-instance")]
    UpdateInstance {
        /// Instance UUID
        uuid: Uuid,
        /// Action: update (default), replace, or delete
        #[arg(long)]
        action: Option<String>,
        /// JSON params object
        #[arg(long)]
        params: Option<String>,
        /// JSON metadata object
        #[arg(long)]
        metadata: Option<String>,
        /// JSON manifests object
        #[arg(long)]
        manifests: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Upgrade an instance to a new image
    #[command(name = "upgrade-instance")]
    UpgradeInstance {
        /// Instance UUID
        uuid: Uuid,
        /// Image UUID to upgrade to (required)
        #[arg(long)]
        image_uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete an instance
    #[command(name = "delete-instance")]
    DeleteInstance {
        /// Instance UUID
        uuid: Uuid,
    },

    /// Get the assembled zone payload for an instance
    #[command(name = "get-instance-payload")]
    GetInstancePayload {
        /// Instance UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Manifests
    // ========================================================================
    /// List manifests
    #[command(name = "list-manifests")]
    ListManifests {
        /// Include master records from remote datacenter
        #[arg(long)]
        include_master: bool,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get a manifest by UUID
    #[command(name = "get-manifest")]
    GetManifest {
        /// Manifest UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a new manifest
    #[command(name = "create-manifest")]
    CreateManifest {
        /// Manifest name (required)
        #[arg(long)]
        name: String,
        /// Path where the config file is placed in the zone (required)
        #[arg(long)]
        path: String,
        /// Template content as JSON string (required)
        #[arg(long)]
        template: String,
        /// Command to run after rendering
        #[arg(long)]
        post_cmd: Option<String>,
        /// Command to run after rendering on Linux
        #[arg(long)]
        post_cmd_linux: Option<String>,
        /// Manifest version (defaults to "1.0.0")
        #[arg(long)]
        version: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete a manifest
    #[command(name = "delete-manifest")]
    DeleteManifest {
        /// Manifest UUID
        uuid: Uuid,
    },

    // ========================================================================
    // Configs
    // ========================================================================
    /// Get assembled config for an instance
    #[command(name = "get-config")]
    GetConfig {
        /// Instance UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },
}

/// Parse a JSON string into a serde_json::Map (for Progenitor-generated types).
fn parse_json_map(s: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
    let v: serde_json::Value = serde_json::from_str(s)?;
    match v {
        serde_json::Value::Object(m) => Ok(m),
        _ => anyhow::bail!("Expected a JSON object"),
    }
}

/// Parse a JSON string into a HashMap<String, String>.
fn parse_string_map(s: &str) -> Result<std::collections::HashMap<String, String>> {
    Ok(serde_json::from_str(s)?)
}

/// Parse a service type string into a ServiceType.
fn parse_service_type(s: &str) -> Result<types::ServiceType> {
    match s {
        "vm" => Ok(types::ServiceType::Vm),
        "agent" => Ok(types::ServiceType::Agent),
        other => anyhow::bail!("Invalid service type: {other}. Must be vm or agent"),
    }
}

/// Parse an action string into an UpdateAction.
fn parse_action(s: &str) -> Result<types::UpdateAction> {
    match s {
        "update" => Ok(types::UpdateAction::Update),
        "replace" => Ok(types::UpdateAction::Replace),
        "delete" => Ok(types::UpdateAction::Delete),
        other => anyhow::bail!("Invalid action: {other}. Must be update, replace, or delete"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(&cli.base_url);

    match cli.command {
        // ====================================================================
        // Ping
        // ====================================================================
        Commands::Ping { raw } => {
            let resp = client
                .ping()
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Ping failed: {e}"))?;
            let ping = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&ping)?);
            } else {
                println!("mode: {}", enum_to_display(&ping.mode));
                println!("storType: {}", enum_to_display(&ping.stor_type));
                println!("storAvailable: {}", ping.stor_available);
            }
        }

        // ====================================================================
        // Mode
        // ====================================================================
        Commands::GetMode { raw } => {
            let resp = client
                .get_mode()
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get mode failed: {e}"))?;
            let mode = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&mode)?);
            } else {
                println!("{}", mode);
            }
        }
        Commands::SetMode { mode, raw: _ } => {
            let sapi_mode = match mode.as_str() {
                "full" => types::SapiMode::Full,
                "proto" => types::SapiMode::Proto,
                other => anyhow::bail!("Invalid mode: {other}. Must be full or proto"),
            };
            let body = types::SetModeBody { mode: sapi_mode };
            client
                .set_mode()
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Set mode failed: {e}"))?;
            // Node.js SAPI returns 204 no content
        }

        // ====================================================================
        // Log Level
        // ====================================================================
        Commands::GetLogLevel { raw } => {
            let resp = client
                .get_log_level()
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get log level failed: {e}"))?;
            let log_resp = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&log_resp)?);
            } else {
                println!("{}", log_resp.level);
            }
        }
        Commands::SetLogLevel { level, raw: _ } => {
            let level_value: serde_json::Value =
                serde_json::from_str(&level).unwrap_or(serde_json::Value::String(level));
            let body = types::SetLogLevelBody { level: level_value };
            client
                .set_log_level()
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Set log level failed: {e}"))?;
            // Node.js SAPI returns empty 200
        }

        // ====================================================================
        // Cache
        // ====================================================================
        Commands::SyncCache => {
            client
                .sync_cache()
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Sync cache failed: {e}"))?;
            println!("Cache synced successfully");
        }

        // ====================================================================
        // Applications
        // ====================================================================
        Commands::ListApplications {
            name,
            owner_uuid,
            include_master,
            raw,
        } => {
            let mut req = client.list_applications();
            if let Some(n) = name {
                req = req.name(n);
            }
            if let Some(ou) = owner_uuid {
                req = req.owner_uuid(ou);
            }
            if include_master {
                req = req.include_master(true);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List applications failed: {e}"))?;
            let apps = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&apps)?);
            } else {
                for app in &apps {
                    println!("{} {}", app.uuid, app.name);
                }
            }
        }
        Commands::GetApplication { uuid, raw } => {
            let resp = client
                .get_application()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get application failed: {e}"))?;
            let app = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&app)?);
            } else {
                println!("uuid: {}", app.uuid);
                println!("name: {}", app.name);
                println!("owner_uuid: {}", app.owner_uuid);
                if let Some(ref m) = app.master {
                    println!("master: {m}");
                }
            }
        }
        Commands::CreateApplication {
            name,
            owner_uuid,
            params,
            metadata,
            metadata_schema,
            manifests,
            raw,
        } => {
            let body = types::CreateApplicationBody {
                name,
                owner_uuid,
                uuid: None,
                params: params.as_deref().map(parse_json_map).transpose()?,
                metadata: metadata.as_deref().map(parse_json_map).transpose()?,
                metadata_schema: metadata_schema.as_deref().map(parse_json_map).transpose()?,
                manifests: manifests.as_deref().map(parse_string_map).transpose()?,
                master: None,
            };
            let resp = client
                .create_application()
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create application failed: {e}"))?;
            let app = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&app)?);
            } else {
                println!("{} {}", app.uuid, app.name);
            }
        }
        Commands::UpdateApplication {
            uuid,
            action,
            params,
            metadata,
            metadata_schema,
            manifests,
            owner_uuid,
            raw,
        } => {
            let body = types::UpdateApplicationBody {
                action: action.as_deref().map(parse_action).transpose()?,
                params: params.as_deref().map(parse_json_map).transpose()?,
                metadata: metadata.as_deref().map(parse_json_map).transpose()?,
                metadata_schema: metadata_schema.as_deref().map(parse_json_map).transpose()?,
                manifests: manifests.as_deref().map(parse_string_map).transpose()?,
                owner_uuid,
            };
            let resp = client
                .update_application()
                .uuid(uuid)
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update application failed: {e}"))?;
            let app = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&app)?);
            } else {
                println!("{} {}", app.uuid, app.name);
            }
        }
        Commands::DeleteApplication { uuid } => {
            client
                .delete_application()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete application failed: {e}"))?;
            println!("Deleted application {uuid}");
        }

        // ====================================================================
        // Services
        // ====================================================================
        Commands::ListServices {
            name,
            application_uuid,
            service_type,
            include_master,
            raw,
        } => {
            let mut req = client.list_services();
            if let Some(n) = name {
                req = req.name(n);
            }
            if let Some(au) = application_uuid {
                req = req.application_uuid(au);
            }
            if let Some(ref st) = service_type {
                req = req.type_(parse_service_type(st)?);
            }
            if include_master {
                req = req.include_master(true);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List services failed: {e}"))?;
            let services = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&services)?);
            } else {
                for svc in &services {
                    let type_str = svc.type_.as_ref().map(enum_to_display).unwrap_or_default();
                    println!("{} {} {}", svc.uuid, svc.name, type_str);
                }
            }
        }
        Commands::GetService { uuid, raw } => {
            let resp = client
                .get_service()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get service failed: {e}"))?;
            let svc = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&svc)?);
            } else {
                println!("uuid: {}", svc.uuid);
                println!("name: {}", svc.name);
                println!("application_uuid: {}", svc.application_uuid);
                if let Some(ref st) = svc.type_ {
                    println!("type: {}", enum_to_display(st));
                }
                if let Some(ref m) = svc.master {
                    println!("master: {m}");
                }
            }
        }
        Commands::CreateService {
            name,
            application_uuid,
            service_type,
            params,
            metadata,
            manifests,
            raw,
        } => {
            let body = types::CreateServiceBody {
                name,
                application_uuid,
                uuid: None,
                type_: service_type
                    .as_deref()
                    .map(parse_service_type)
                    .transpose()?,
                params: params.as_deref().map(parse_json_map).transpose()?,
                metadata: metadata.as_deref().map(parse_json_map).transpose()?,
                manifests: manifests.as_deref().map(parse_string_map).transpose()?,
                master: None,
            };
            let resp = client
                .create_service()
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create service failed: {e}"))?;
            let svc = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&svc)?);
            } else {
                println!("{} {}", svc.uuid, svc.name);
            }
        }
        Commands::UpdateService {
            uuid,
            action,
            params,
            metadata,
            manifests,
            raw,
        } => {
            let body = types::UpdateServiceBody {
                action: action.as_deref().map(parse_action).transpose()?,
                params: params.as_deref().map(parse_json_map).transpose()?,
                metadata: metadata.as_deref().map(parse_json_map).transpose()?,
                manifests: manifests.as_deref().map(parse_string_map).transpose()?,
            };
            let resp = client
                .update_service()
                .uuid(uuid)
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update service failed: {e}"))?;
            let svc = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&svc)?);
            } else {
                println!("{} {}", svc.uuid, svc.name);
            }
        }
        Commands::DeleteService { uuid } => {
            client
                .delete_service()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete service failed: {e}"))?;
            println!("Deleted service {uuid}");
        }

        // ====================================================================
        // Instances
        // ====================================================================
        Commands::ListInstances {
            service_uuid,
            instance_type,
            include_master,
            raw,
        } => {
            let mut req = client.list_instances();
            if let Some(su) = service_uuid {
                req = req.service_uuid(su);
            }
            if let Some(ref it) = instance_type {
                req = req.type_(parse_service_type(it)?);
            }
            if include_master {
                req = req.include_master(true);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List instances failed: {e}"))?;
            let instances = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&instances)?);
            } else {
                for inst in &instances {
                    let type_str = inst.type_.as_ref().map(enum_to_display).unwrap_or_default();
                    println!("{} {} {}", inst.uuid, inst.service_uuid, type_str);
                }
            }
        }
        Commands::GetInstance { uuid, raw } => {
            let resp = client
                .get_instance()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get instance failed: {e}"))?;
            let inst = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&inst)?);
            } else {
                println!("uuid: {}", inst.uuid);
                println!("service_uuid: {}", inst.service_uuid);
                if let Some(ref it) = inst.type_ {
                    println!("type: {}", enum_to_display(it));
                }
                if let Some(ref m) = inst.master {
                    println!("master: {m}");
                }
                if let Some(ref ju) = inst.job_uuid {
                    println!("job_uuid: {ju}");
                }
            }
        }
        Commands::CreateInstance {
            service_uuid,
            uuid,
            async_create,
            params,
            metadata,
            manifests,
            raw,
        } => {
            let body = types::CreateInstanceBody {
                service_uuid,
                uuid,
                params: params.as_deref().map(parse_json_map).transpose()?,
                metadata: metadata.as_deref().map(parse_json_map).transpose()?,
                manifests: manifests.as_deref().map(parse_string_map).transpose()?,
                master: None,
            };
            let mut req = client.create_instance().body(body);
            if async_create {
                req = req.async_(true);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create instance failed: {e}"))?;
            let inst = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&inst)?);
            } else {
                println!("{} {}", inst.uuid, inst.service_uuid);
                if let Some(ref ju) = inst.job_uuid {
                    println!("job_uuid: {ju}");
                }
            }
        }
        Commands::UpdateInstance {
            uuid,
            action,
            params,
            metadata,
            manifests,
            raw,
        } => {
            let body = types::UpdateInstanceBody {
                action: action.as_deref().map(parse_action).transpose()?,
                params: params.as_deref().map(parse_json_map).transpose()?,
                metadata: metadata.as_deref().map(parse_json_map).transpose()?,
                manifests: manifests.as_deref().map(parse_string_map).transpose()?,
            };
            let resp = client
                .update_instance()
                .uuid(uuid)
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update instance failed: {e}"))?;
            let inst = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&inst)?);
            } else {
                println!("{} {}", inst.uuid, inst.service_uuid);
            }
        }
        Commands::UpgradeInstance {
            uuid,
            image_uuid,
            raw,
        } => {
            let body = types::UpgradeInstanceBody { image_uuid };
            let resp = client
                .upgrade_instance()
                .uuid(uuid)
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Upgrade instance failed: {e}"))?;
            let inst = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&inst)?);
            } else {
                println!("{} {}", inst.uuid, inst.service_uuid);
            }
        }
        Commands::DeleteInstance { uuid } => {
            client
                .delete_instance()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete instance failed: {e}"))?;
            println!("Deleted instance {uuid}");
        }
        Commands::GetInstancePayload { uuid, raw: _ } => {
            let resp = client
                .get_instance_payload()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get instance payload failed: {e}"))?;
            let payload = resp.into_inner();
            // Payload is always freeform JSON, so always output as JSON
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }

        // ====================================================================
        // Manifests
        // ====================================================================
        Commands::ListManifests {
            include_master,
            raw,
        } => {
            let mut req = client.list_manifests();
            if include_master {
                req = req.include_master(true);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List manifests failed: {e}"))?;
            let manifests = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&manifests)?);
            } else {
                for m in &manifests {
                    println!("{} {} {}", m.uuid, m.name, m.path);
                }
            }
        }
        Commands::GetManifest { uuid, raw } => {
            let resp = client
                .get_manifest()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get manifest failed: {e}"))?;
            let manifest = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else {
                println!("uuid: {}", manifest.uuid);
                println!("name: {}", manifest.name);
                println!("path: {}", manifest.path);
                if let Some(ref v) = manifest.version {
                    println!("version: {v}");
                }
                if let Some(ref pc) = manifest.post_cmd {
                    println!("post_cmd: {pc}");
                }
                if let Some(ref pcl) = manifest.post_cmd_linux {
                    println!("post_cmd_linux: {pcl}");
                }
            }
        }
        Commands::CreateManifest {
            name,
            path,
            template,
            post_cmd,
            post_cmd_linux,
            version,
            raw,
        } => {
            let template_value: serde_json::Value =
                serde_json::from_str(&template).unwrap_or(serde_json::Value::String(template));
            let body = types::CreateManifestBody {
                name,
                path,
                template: template_value,
                uuid: None,
                post_cmd,
                post_cmd_linux,
                version,
                master: None,
            };
            let resp = client
                .create_manifest()
                .body(body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create manifest failed: {e}"))?;
            let manifest = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else {
                println!("{} {} {}", manifest.uuid, manifest.name, manifest.path);
            }
        }
        Commands::DeleteManifest { uuid } => {
            client
                .delete_manifest()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete manifest failed: {e}"))?;
            println!("Deleted manifest {uuid}");
        }

        // ====================================================================
        // Configs
        // ====================================================================
        Commands::GetConfig { uuid, raw: _ } => {
            let resp = client
                .get_config()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get config failed: {e}"))?;
            let config = resp.into_inner();
            // Config is always freeform JSON, so always output as JSON
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
    }

    Ok(())
}
