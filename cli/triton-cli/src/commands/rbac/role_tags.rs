// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! RBAC role tags management commands

use std::io::Write;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::client::AnyClient;
use crate::output::json;
use crate::{dispatch, dispatch_with_types};

use super::common::resolve_user;

/// Parse resource ID as UUID, returning an error if invalid
fn parse_resource_uuid(resource_id: &str, resource_type: &RoleTagResource) -> Result<uuid::Uuid> {
    resource_id
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid UUID for {}: {}", resource_type, resource_id))
}

/// Resource types that support role tags
#[derive(Clone, Copy, Debug, clap::ValueEnum, strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum RoleTagResource {
    /// Instance/machine
    Instance,
    /// Image
    Image,
    /// Network
    Network,
    /// Package
    Package,
    /// SSH key
    Key,
    /// Firewall rule
    Fwrule,
    /// RBAC user
    User,
    /// RBAC role
    Role,
    /// RBAC policy
    Policy,
}

#[derive(Subcommand, Clone)]
pub enum RoleTagsCommand {
    /// Set role tags on a resource (replaces existing tags)
    Set(RoleTagsSetArgs),
    /// Add role tags to a resource
    Add(RoleTagsAddArgs),
    /// Remove role tags from a resource
    #[command(visible_alias = "rm")]
    Remove(RoleTagsRemoveArgs),
    /// Clear all role tags from a resource
    Clear(RoleTagsClearArgs),
    /// Edit role tags in $EDITOR
    #[command(visible_alias = "e")]
    Edit(RoleTagsEditArgs),
}

#[derive(Args, Clone)]
pub struct RoleTagsSetArgs {
    /// Resource type
    #[arg(value_enum)]
    pub resource_type: RoleTagResource,
    /// Resource ID or name
    pub resource: String,
    /// Role names to set (replaces all existing tags)
    pub roles: Vec<String>,
    /// Skip confirmation
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct RoleTagsAddArgs {
    /// Resource type
    #[arg(value_enum)]
    pub resource_type: RoleTagResource,
    /// Resource ID or name
    pub resource: String,
    /// Role name to add
    pub role: String,
}

#[derive(Args, Clone)]
pub struct RoleTagsRemoveArgs {
    /// Resource type
    #[arg(value_enum)]
    pub resource_type: RoleTagResource,
    /// Resource ID or name
    pub resource: String,
    /// Role name to remove
    pub role: String,
    /// Skip confirmation
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct RoleTagsClearArgs {
    /// Resource type
    #[arg(value_enum)]
    pub resource_type: RoleTagResource,
    /// Resource ID or name
    pub resource: String,
    /// Skip confirmation
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct RoleTagsEditArgs {
    /// Resource type
    #[arg(value_enum)]
    pub resource_type: RoleTagResource,
    /// Resource ID or name
    pub resource: String,
}

impl RoleTagsCommand {
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
        match self {
            Self::Set(args) => role_tags_set(args, client, use_json).await,
            Self::Add(args) => role_tags_add(args, client, use_json).await,
            Self::Remove(args) => role_tags_remove(args, client, use_json).await,
            Self::Clear(args) => role_tags_clear(args, client, use_json).await,
            Self::Edit(args) => role_tags_edit(args, client, use_json).await,
        }
    }
}

/// Set role tags on a resource (replaces all existing tags).
///
/// Returns the (name, role_tag) pair from the server response.
async fn role_tags_set(args: RoleTagsSetArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt(format!(
                "Set role tags on {} \"{}\"?",
                args.resource_type, args.resource
            ))
            .default(false)
            .interact()?
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    let account = client.effective_account();
    let resource_id = resolve_resource_id(&args.resource_type, &args.resource, client).await?;
    let roles = args.roles.clone();

    // Each per-crate ReplaceRoleTagsRequest has the same wire shape; build
    // the body via typed dispatch and capture the (name, role_tag) pair.
    let (name, role_tag): (String, Vec<String>) = match args.resource_type {
        RoleTagResource::Instance => {
            let uuid = parse_resource_uuid(&resource_id, &args.resource_type)?;
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_machine_role_tags()
                    .account(account)
                    .machine(uuid)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
        RoleTagResource::Image => {
            let uuid = parse_resource_uuid(&resource_id, &args.resource_type)?;
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_image_role_tags()
                    .account(account)
                    .dataset(uuid)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
        RoleTagResource::Network => {
            let uuid = parse_resource_uuid(&resource_id, &args.resource_type)?;
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_network_role_tags()
                    .account(account)
                    .network(uuid)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
        RoleTagResource::Package => {
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_package_role_tags()
                    .account(account)
                    .package(&resource_id)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
        RoleTagResource::Key => {
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_key_role_tags()
                    .account(account)
                    .name(&resource_id)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
        RoleTagResource::Fwrule => {
            let uuid = parse_resource_uuid(&resource_id, &args.resource_type)?;
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_fwrule_role_tags()
                    .account(account)
                    .id(uuid)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
        RoleTagResource::User => {
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_user_role_tags()
                    .account(account)
                    .uuid(&resource_id)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
        RoleTagResource::Role => {
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_role_role_tags()
                    .account(account)
                    .role(&resource_id)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
        RoleTagResource::Policy => {
            dispatch_with_types!(client, |c, t| {
                let request = t::ReplaceRoleTagsRequest {
                    role_tag: roles.clone(),
                };
                let resp = c
                    .inner()
                    .replace_policy_role_tags()
                    .account(account)
                    .policy(&resource_id)
                    .body(request)
                    .send()
                    .await?
                    .into_inner();
                (resp.name.clone(), resp.role_tag.clone())
            })
        }
    };

    if use_json {
        let obj = serde_json::json!({
            "name": name,
            "role-tag": role_tag,
        });
        json::print_json(&obj)?;
    } else {
        println!("Set role tags on {}:", name);
        if role_tag.is_empty() {
            println!("  (no role tags)");
        } else {
            for tag in &role_tag {
                println!("  - {}", tag);
            }
        }
    }

    Ok(())
}

/// Add a role tag to a resource (preserves existing tags)
async fn role_tags_add(args: RoleTagsAddArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let resource_id = resolve_resource_id(&args.resource_type, &args.resource, client).await?;

    // GET current tags
    let mut current_tags = get_current_role_tags(&args.resource_type, &resource_id, client).await?;

    // Add new tag if not already present
    if !current_tags.contains(&args.role) {
        current_tags.push(args.role.clone());
    }

    // SET updated tags
    let set_args = RoleTagsSetArgs {
        resource_type: args.resource_type,
        resource: resource_id,
        roles: current_tags,
        force: true,
    };
    role_tags_set(set_args, client, use_json).await
}

/// Remove a role tag from a resource
async fn role_tags_remove(
    args: RoleTagsRemoveArgs,
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt(format!(
                "Remove role tag \"{}\" from {} \"{}\"?",
                args.role, args.resource_type, args.resource
            ))
            .default(false)
            .interact()?
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    let resource_id = resolve_resource_id(&args.resource_type, &args.resource, client).await?;

    // GET current tags
    let mut current_tags = get_current_role_tags(&args.resource_type, &resource_id, client).await?;

    // Remove the tag
    current_tags.retain(|t| t != &args.role);

    // SET updated tags
    let set_args = RoleTagsSetArgs {
        resource_type: args.resource_type,
        resource: resource_id,
        roles: current_tags,
        force: true,
    };
    role_tags_set(set_args, client, use_json).await
}

/// Clear all role tags from a resource
async fn role_tags_clear(
    args: RoleTagsClearArgs,
    client: &AnyClient,
    use_json: bool,
) -> Result<()> {
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt(format!(
                "Clear all role tags from {} \"{}\"?",
                args.resource_type, args.resource
            ))
            .default(false)
            .interact()?
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    let set_args = RoleTagsSetArgs {
        resource_type: args.resource_type,
        resource: args.resource,
        roles: vec![],
        force: true,
    };
    role_tags_set(set_args, client, use_json).await
}

/// Edit role tags in $EDITOR
async fn role_tags_edit(args: RoleTagsEditArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let resource_id = resolve_resource_id(&args.resource_type, &args.resource, client).await?;

    // GET current tags
    let current_tags = get_current_role_tags(&args.resource_type, &resource_id, client).await?;

    // Create text representation (one tag per line)
    let orig_text = role_tags_to_text(&current_tags);

    // Create temp file and write current tags
    let mut temp_file = tempfile::Builder::new()
        .prefix("role-tags-")
        .suffix(".txt")
        .tempfile()?;
    writeln!(
        temp_file,
        "# Edit role tags for {} (one per line)",
        args.resource
    )?;
    writeln!(temp_file, "# Lines starting with # are ignored")?;
    writeln!(
        temp_file,
        "# Save and exit to apply changes, or exit without saving to cancel"
    )?;
    writeln!(temp_file)?;
    write!(temp_file, "{}", orig_text)?;
    temp_file.flush()?;

    // Get editor from environment
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    // Open editor
    let status = std::process::Command::new(&editor)
        .arg(temp_file.path())
        .status()?;

    if !status.success() {
        return Err(anyhow::anyhow!("Editor exited with error"));
    }

    // Read back the edited file
    let edited_content = tokio::fs::read_to_string(temp_file.path()).await?;
    let edited_tags = text_to_role_tags(&edited_content);

    // Check if anything changed
    let edited_text = role_tags_to_text(&edited_tags);
    if edited_text == orig_text {
        println!("No changes made.");
        return Ok(());
    }

    // SET updated tags
    let set_args = RoleTagsSetArgs {
        resource_type: args.resource_type,
        resource: resource_id,
        roles: edited_tags,
        force: true,
    };
    role_tags_set(set_args, client, use_json).await
}

/// Convert role tags to text representation (one per line, sorted)
fn role_tags_to_text(tags: &[String]) -> String {
    let mut sorted_tags = tags.to_vec();
    sorted_tags.sort();
    sorted_tags.join("\n")
}

/// Parse text back to role tags (strips comments, whitespace)
fn text_to_role_tags(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_string())
        .collect()
}

/// Resolve resource identifier to UUID or name as needed by the API
async fn resolve_resource_id(
    resource_type: &RoleTagResource,
    resource: &str,
    client: &AnyClient,
) -> Result<String> {
    let account = client.effective_account();

    match resource_type {
        RoleTagResource::Instance => {
            // Resolve instance name to UUID if needed
            if let Ok(uuid) = uuid::Uuid::parse_str(resource) {
                Ok(uuid.to_string())
            } else {
                // Search for instance by name; return value is a JSON Value so
                // we can read id out without sharing the per-crate Machine type.
                let machines: serde_json::Value = dispatch!(client, |c| {
                    let resp = c
                        .inner()
                        .list_machines()
                        .account(account)
                        .name(resource)
                        .send()
                        .await?
                        .into_inner();
                    serde_json::to_value(&resp)?
                });
                let arr = machines.as_array().cloned().unwrap_or_default();
                if arr.is_empty() {
                    Err(crate::errors::ResourceNotFoundError(format!(
                        "Instance not found: {}",
                        resource
                    ))
                    .into())
                } else if arr.len() > 1 {
                    Err(anyhow::anyhow!(
                        "Multiple instances found with name '{}'. Please use UUID.",
                        resource
                    ))
                } else {
                    let id = arr[0]
                        .get("id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing id on matched instance"))?;
                    Ok(id.to_string())
                }
            }
        }
        RoleTagResource::Image => {
            // Image can be UUID or name@version - just pass through
            Ok(resource.to_string())
        }
        RoleTagResource::Network => {
            // Network can be UUID or name
            if let Ok(uuid) = uuid::Uuid::parse_str(resource) {
                Ok(uuid.to_string())
            } else {
                // Search for network by name via JSON
                let networks: serde_json::Value = dispatch!(client, |c| {
                    let resp = c
                        .inner()
                        .list_networks()
                        .account(account)
                        .send()
                        .await?
                        .into_inner();
                    serde_json::to_value(&resp)?
                });
                if let Some(arr) = networks.as_array() {
                    for net in arr {
                        if net.get("name").and_then(|v| v.as_str()) == Some(resource)
                            && let Some(id) = net.get("id").and_then(|v| v.as_str())
                        {
                            return Ok(id.to_string());
                        }
                    }
                }
                Err(crate::errors::ResourceNotFoundError(format!(
                    "Network not found: {}",
                    resource
                ))
                .into())
            }
        }
        RoleTagResource::Package => {
            // Package can be name or UUID
            Ok(resource.to_string())
        }
        RoleTagResource::Key => {
            // Key is by name
            Ok(resource.to_string())
        }
        RoleTagResource::Fwrule => {
            // Firewall rule is by UUID - normalize casing
            if let Ok(uuid) = uuid::Uuid::parse_str(resource) {
                Ok(uuid.to_string())
            } else {
                Ok(resource.to_string())
            }
        }
        RoleTagResource::User => {
            // User can be login or UUID
            resolve_user(resource, client).await
        }
        RoleTagResource::Role => {
            // Role is by name
            Ok(resource.to_string())
        }
        RoleTagResource::Policy => {
            // Policy is by name
            Ok(resource.to_string())
        }
    }
}

/// Get current role tags from a resource
async fn get_current_role_tags(
    resource_type: &RoleTagResource,
    resource_id: &str,
    client: &AnyClient,
) -> Result<Vec<String>> {
    let account = client.effective_account();

    // Each GET endpoint returns a typed object; we extract role_tag via JSON
    // so the per-crate types stay inside the dispatch arm.
    let value: serde_json::Value = match resource_type {
        RoleTagResource::Instance => {
            let uuid = parse_resource_uuid(resource_id, resource_type)?;
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_machine()
                    .account(account)
                    .machine(uuid)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
        RoleTagResource::Image => {
            let uuid = parse_resource_uuid(resource_id, resource_type)?;
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_image()
                    .account(account)
                    .dataset(uuid)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
        RoleTagResource::Network => {
            let uuid = parse_resource_uuid(resource_id, resource_type)?;
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_network()
                    .account(account)
                    .network(uuid)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
        RoleTagResource::Package => {
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_package()
                    .account(account)
                    .package(resource_id)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
        RoleTagResource::Key => {
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_key()
                    .account(account)
                    .name(resource_id)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
        RoleTagResource::Fwrule => {
            let uuid = parse_resource_uuid(resource_id, resource_type)?;
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_firewall_rule()
                    .account(account)
                    .id(uuid)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
        RoleTagResource::User => {
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_user()
                    .account(account)
                    .uuid(resource_id)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
        RoleTagResource::Role => {
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_role()
                    .account(account)
                    .role(resource_id)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
        RoleTagResource::Policy => {
            dispatch!(client, |c| {
                let resp = c
                    .inner()
                    .get_policy()
                    .account(account)
                    .policy(resource_id)
                    .send()
                    .await?
                    .into_inner();
                serde_json::to_value(&resp)?
            })
        }
    };

    // Extract role_tag or role-tag, defaulting to an empty list
    let tags = value
        .get("role-tag")
        .or_else(|| value.get("role_tag"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(tags)
}
