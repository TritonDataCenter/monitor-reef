// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC role tags management commands

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
    #[command(alias = "rm")]
    Remove(RoleTagsRemoveArgs),
    /// Clear all role tags from a resource
    Clear(RoleTagsClearArgs),
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

impl RoleTagsCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Set(args) => role_tags_set(args, client, use_json).await,
            Self::Add(args) => role_tags_add(args, client, use_json).await,
            Self::Remove(args) => role_tags_remove(args, client, use_json).await,
            Self::Clear(args) => role_tags_clear(args, client, use_json).await,
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

    let response = match args.resource_type {
        RoleTagResource::Instance => client
            .inner()
            .replace_machine_role_tags()
            .account(account)
            .machine(&resource_id)
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Image => client
            .inner()
            .replace_image_role_tags()
            .account(account)
            .dataset(&resource_id)
            .body(request)
            .send()
            .await?
            .into_inner(),
        RoleTagResource::Network => client
            .inner()
            .replace_network_role_tags()
            .account(account)
            .network(&resource_id)
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
            .id(&resource_id)
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
///
/// Note: The API only supports replacing all tags, so we need to set
/// the new tag list which includes the added tag. However, since we
/// can't reliably GET current tags, this command simply sets the single tag.
/// For proper add behavior, use a management tool that tracks state.
async fn role_tags_add(args: RoleTagsAddArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    // Since we can't GET current tags, we warn the user
    eprintln!(
        "Warning: Cannot retrieve current role tags. This will set the role tag to only '{}'. Use 'set' command to specify all desired tags.",
        args.role
    );

    let set_args = RoleTagsSetArgs {
        resource_type: args.resource_type,
        resource: args.resource,
        roles: vec![args.role],
    };
    role_tags_set(set_args, client, use_json).await
}

/// Remove a role tag from a resource
///
/// Note: Since we can't GET current tags, this clears all tags.
async fn role_tags_remove(
    args: RoleTagsRemoveArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    eprintln!(
        "Warning: Cannot retrieve current role tags. This will clear all role tags. Use 'set' command to specify desired tags instead."
    );

    let set_args = RoleTagsSetArgs {
        resource_type: args.resource_type,
        resource: args.resource,
        roles: vec![],
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
