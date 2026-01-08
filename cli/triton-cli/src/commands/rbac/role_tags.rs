// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! RBAC role tags management commands

use std::io::Write;

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json;

use super::common::resolve_user;

/// Resource types that support role tags
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
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
}

#[derive(Args, Clone)]
pub struct RoleTagsClearArgs {
    /// Resource type
    #[arg(value_enum)]
    pub resource_type: RoleTagResource,
    /// Resource ID or name
    pub resource: String,
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
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Set(args) => role_tags_set(args, client, use_json).await,
            Self::Add(args) => role_tags_add(args, client, use_json).await,
            Self::Remove(args) => role_tags_remove(args, client, use_json).await,
            Self::Clear(args) => role_tags_clear(args, client, use_json).await,
            Self::Edit(args) => role_tags_edit(args, client, use_json).await,
        }
    }
}

/// Set role tags on a resource (replaces all existing tags)
async fn role_tags_set(args: RoleTagsSetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let resource_id = resolve_resource_id(&args.resource_type, &args.resource, client).await?;

    let request = cloudapi_client::types::ReplaceRoleTagsRequest {
        role_tag: args.roles.clone(),
    };

    // Parse resource_id to UUID for resources that require it
    let resource_uuid: Option<uuid::Uuid> = match args.resource_type {
        RoleTagResource::Instance
        | RoleTagResource::Image
        | RoleTagResource::Network
        | RoleTagResource::Fwrule => Some(resource_id.parse().map_err(|_| {
            anyhow::anyhow!("Invalid UUID for {:?}: {}", args.resource_type, resource_id)
        })?),
        _ => None,
    };

    let response = match args.resource_type {
        RoleTagResource::Instance => client
            .inner()
            .replace_machine_role_tags()
            .account(account)
            .machine(resource_uuid.unwrap())
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Image => client
            .inner()
            .replace_image_role_tags()
            .account(account)
            .dataset(resource_uuid.unwrap())
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Network => client
            .inner()
            .replace_network_role_tags()
            .account(account)
            .network(resource_uuid.unwrap())
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Package => client
            .inner()
            .replace_package_role_tags()
            .account(account)
            .package(&resource_id)
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Key => client
            .inner()
            .replace_key_role_tags()
            .account(account)
            .name(&resource_id)
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Fwrule => client
            .inner()
            .replace_fwrule_role_tags()
            .account(account)
            .id(resource_uuid.unwrap())
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::User => client
            .inner()
            .replace_user_role_tags()
            .account(account)
            .uuid(&resource_id)
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Role => client
            .inner()
            .replace_role_role_tags()
            .account(account)
            .role(&resource_id)
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Policy => client
            .inner()
            .replace_policy_role_tags()
            .account(account)
            .policy(&resource_id)
            .body(request)
            .send()
            .await?
            .into_inner(),
    };

    if use_json {
        json::print_json(&response)?;
    } else {
        println!("Set role tags on {}:", response.name);
        if response.role_tag.is_empty() {
            println!("  (no role tags)");
        } else {
            for tag in &response.role_tag {
                println!("  - {}", tag);
            }
        }
    }

    Ok(())
}

/// Add a role tag to a resource (preserves existing tags)
async fn role_tags_add(args: RoleTagsAddArgs, client: &TypedClient, use_json: bool) -> Result<()> {
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
    };
    role_tags_set(set_args, client, use_json).await
}

/// Remove a role tag from a resource
async fn role_tags_remove(
    args: RoleTagsRemoveArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
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
    };
    role_tags_set(set_args, client, use_json).await
}

/// Clear all role tags from a resource
async fn role_tags_clear(
    args: RoleTagsClearArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let set_args = RoleTagsSetArgs {
        resource_type: args.resource_type,
        resource: args.resource,
        roles: vec![],
    };
    role_tags_set(set_args, client, use_json).await
}

/// Edit role tags in $EDITOR
async fn role_tags_edit(
    args: RoleTagsEditArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
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
    let edited_content = std::fs::read_to_string(temp_file.path())?;
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
    client: &TypedClient,
) -> Result<String> {
    let account = &client.auth_config().account;

    match resource_type {
        RoleTagResource::Instance => {
            // Resolve instance name to UUID if needed
            if uuid::Uuid::parse_str(resource).is_ok() {
                Ok(resource.to_string())
            } else {
                // Search for instance by name
                let response = client
                    .inner()
                    .list_machines()
                    .account(account)
                    .name(resource)
                    .send()
                    .await?;
                let machines = response.into_inner();
                if machines.is_empty() {
                    Err(anyhow::anyhow!("Instance not found: {}", resource))
                } else if machines.len() > 1 {
                    Err(anyhow::anyhow!(
                        "Multiple instances found with name '{}'. Please use UUID.",
                        resource
                    ))
                } else {
                    Ok(machines[0].id.to_string())
                }
            }
        }
        RoleTagResource::Image => {
            // Image can be UUID or name@version - just pass through
            Ok(resource.to_string())
        }
        RoleTagResource::Network => {
            // Network can be UUID or name
            if uuid::Uuid::parse_str(resource).is_ok() {
                Ok(resource.to_string())
            } else {
                // Search for network by name
                let response = client
                    .inner()
                    .list_networks()
                    .account(account)
                    .send()
                    .await?;
                let networks = response.into_inner();
                for net in &networks {
                    if net.name == resource {
                        return Ok(net.id.to_string());
                    }
                }
                Err(anyhow::anyhow!("Network not found: {}", resource))
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
            // Firewall rule is by UUID
            Ok(resource.to_string())
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
    client: &TypedClient,
) -> Result<Vec<String>> {
    let account = &client.auth_config().account;

    // Parse to UUID for resources that require it
    let resource_uuid: Option<uuid::Uuid> = match resource_type {
        RoleTagResource::Instance
        | RoleTagResource::Image
        | RoleTagResource::Network
        | RoleTagResource::Fwrule => Some(resource_id.parse().map_err(|_| {
            anyhow::anyhow!("Invalid UUID for {:?}: {}", resource_type, resource_id)
        })?),
        _ => None,
    };

    match resource_type {
        RoleTagResource::Instance => {
            let machine = client
                .inner()
                .get_machine()
                .account(account)
                .machine(resource_uuid.unwrap())
                .send()
                .await?
                .into_inner();
            Ok(machine.role_tag.unwrap_or_default())
        }
        RoleTagResource::Image => {
            let image = client
                .inner()
                .get_image()
                .account(account)
                .dataset(resource_uuid.unwrap())
                .send()
                .await?
                .into_inner();
            Ok(image.role_tag.unwrap_or_default())
        }
        RoleTagResource::Network => {
            let network = client
                .inner()
                .get_network()
                .account(account)
                .network(resource_uuid.unwrap())
                .send()
                .await?
                .into_inner();
            Ok(network.role_tag.unwrap_or_default())
        }
        RoleTagResource::Package => {
            let package = client
                .inner()
                .get_package()
                .account(account)
                .package(resource_id)
                .send()
                .await?
                .into_inner();
            Ok(package.role_tag.unwrap_or_default())
        }
        RoleTagResource::Key => {
            let key = client
                .inner()
                .get_key()
                .account(account)
                .name(resource_id)
                .send()
                .await?
                .into_inner();
            Ok(key.role_tag.unwrap_or_default())
        }
        RoleTagResource::Fwrule => {
            let rule = client
                .inner()
                .get_firewall_rule()
                .account(account)
                .id(resource_uuid.unwrap())
                .send()
                .await?
                .into_inner();
            Ok(rule.role_tag.unwrap_or_default())
        }
        RoleTagResource::User => {
            let user = client
                .inner()
                .get_user()
                .account(account)
                .uuid(resource_id)
                .send()
                .await?
                .into_inner();
            Ok(user.role_tag.unwrap_or_default())
        }
        RoleTagResource::Role => {
            let role = client
                .inner()
                .get_role()
                .account(account)
                .role(resource_id)
                .send()
                .await?
                .into_inner();
            Ok(role.role_tag.unwrap_or_default())
        }
        RoleTagResource::Policy => {
            let policy = client
                .inner()
                .get_policy()
                .account(account)
                .policy(resource_id)
                .send()
                .await?
                .into_inner();
            Ok(policy.role_tag.unwrap_or_default())
        }
    }
}
