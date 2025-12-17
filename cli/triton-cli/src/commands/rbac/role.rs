// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC role management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use serde::Deserialize;

use crate::output::{json, table};

use super::editor;

/// Role subcommands (modern pattern)
#[derive(Subcommand, Clone)]
pub enum RoleSubcommand {
    /// List RBAC roles
    #[command(alias = "ls")]
    List,
    /// Get role details
    Get(RoleGetArgs),
    /// Create role
    Create(RoleCreateArgs),
    /// Update role
    Update(RoleUpdateArgs),
    /// Delete role(s)
    #[command(alias = "rm")]
    Delete(RoleDeleteArgs),
}

/// RBAC role command supporting both subcommands and action flags
///
/// This command supports two patterns for compatibility:
///
/// Modern (subcommand) pattern:
///   triton rbac role list
///   triton rbac role get ROLE
///   triton rbac role create NAME --policy ...
///   triton rbac role delete ROLE
///
/// Legacy (action flag) pattern:
///   triton rbac role ROLE           # show role (default)
///   triton rbac role -a [FILE]      # add role from file or stdin
///   triton rbac role -e ROLE        # edit role in $EDITOR
///   triton rbac role -d ROLE...     # delete role(s)
#[derive(Args, Clone)]
#[command(args_conflicts_with_subcommands = true)]
pub struct RbacRoleCommand {
    #[command(subcommand)]
    pub command: Option<RoleSubcommand>,

    /// Add a new role (legacy compat: read from FILE, "-" for stdin, or interactive)
    #[arg(short = 'a', long = "add", conflicts_with_all = ["delete", "edit"])]
    pub add: bool,

    /// Edit role in $EDITOR (legacy compat)
    #[arg(short = 'e', long = "edit", conflicts_with_all = ["add", "delete"])]
    pub edit: bool,

    /// Delete role(s) (legacy compat)
    #[arg(short = 'd', long = "delete", conflicts_with_all = ["add", "edit"])]
    pub delete: bool,

    /// Skip confirmation (for delete)
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Role(s) or file argument
    /// For show: ROLE name/uuid
    /// For add: optional FILE path (or "-" for stdin)
    /// For edit: ROLE name/uuid
    /// For delete: one or more ROLE name/uuid
    #[arg(trailing_var_arg = true)]
    pub args: Vec<String>,
}

#[derive(Args, Clone)]
pub struct RoleGetArgs {
    /// Role name or UUID
    pub role: String,
}

#[derive(Args, Clone)]
pub struct RoleCreateArgs {
    /// Role name
    pub name: String,
    /// Policies to attach (can be specified multiple times)
    #[arg(long)]
    pub policy: Vec<String>,
    /// Members (user logins, can be specified multiple times)
    #[arg(long)]
    pub member: Vec<String>,
    /// Default members (user logins, can be specified multiple times)
    #[arg(long)]
    pub default_member: Vec<String>,
}

#[derive(Args, Clone)]
pub struct RoleUpdateArgs {
    /// Role name or UUID
    pub role: String,
    /// New name
    #[arg(long)]
    pub name: Option<String>,
    /// Policies (replaces existing)
    #[arg(long)]
    pub policy: Vec<String>,
    /// Members (replaces existing)
    #[arg(long)]
    pub member: Vec<String>,
    /// Default members (replaces existing)
    #[arg(long)]
    pub default_member: Vec<String>,
}

#[derive(Args, Clone)]
pub struct RoleDeleteArgs {
    /// Role name(s) or UUID(s)
    pub roles: Vec<String>,
    /// Skip confirmation
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

impl RbacRoleCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        // If a subcommand is provided, use the modern pattern
        if let Some(cmd) = self.command {
            return match cmd {
                RoleSubcommand::List => list_roles(client, use_json).await,
                RoleSubcommand::Get(args) => get_role(args, client, use_json).await,
                RoleSubcommand::Create(args) => create_role(args, client, use_json).await,
                RoleSubcommand::Update(args) => update_role(args, client, use_json).await,
                RoleSubcommand::Delete(args) => delete_roles(args, client).await,
            };
        }

        // Legacy action flag pattern
        if self.add {
            // -a/--add: add role from file or stdin
            let file = self.args.first().map(|s| s.as_str());
            add_role_from_file(file, client, use_json).await
        } else if self.edit {
            // -e/--edit: edit role in $EDITOR
            if self.args.is_empty() {
                anyhow::bail!("ROLE argument required for edit");
            }
            edit_role_in_editor(&self.args[0], client).await
        } else if self.delete {
            // -d/--delete: delete role(s)
            if self.args.is_empty() {
                anyhow::bail!("ROLE argument(s) required for delete");
            }
            let args = RoleDeleteArgs {
                roles: self.args,
                force: self.yes,
            };
            delete_roles(args, client).await
        } else if !self.args.is_empty() {
            // Default: show role
            let args = RoleGetArgs {
                role: self.args[0].clone(),
            };
            get_role(args, client, use_json).await
        } else {
            // No args and no subcommand - show usage hint
            anyhow::bail!(
                "Usage: triton rbac role <SUBCOMMAND>\n\
                 Or:    triton rbac role ROLE           (show role)\n\
                 Or:    triton rbac role -a [FILE]      (add role)\n\
                 Or:    triton rbac role -e ROLE        (edit role)\n\
                 Or:    triton rbac role -d ROLE...     (delete roles)\n\n\
                 Run 'triton rbac role --help' for more information"
            );
        }
    }
}

pub async fn list_roles(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client.inner().list_roles().account(account).send().await?;

    let roles = response.into_inner();

    if use_json {
        json::print_json(&roles)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "POLICIES", "MEMBERS"]);
        for role in &roles {
            tbl.add_row(vec![
                &role.id.to_string()[..8],
                &role.name,
                &role.policies.join(", "),
                &role.members.join(", "),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_role(args: RoleGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .get_role()
        .account(account)
        .role(&args.role)
        .send()
        .await?;

    let role = response.into_inner();

    if use_json {
        json::print_json(&role)?;
    } else {
        println!("ID:              {}", role.id);
        println!("Name:            {}", role.name);
        println!(
            "Policies:        {}",
            if role.policies.is_empty() {
                "-".to_string()
            } else {
                role.policies.join(", ")
            }
        );
        println!(
            "Members:         {}",
            if role.members.is_empty() {
                "-".to_string()
            } else {
                role.members.join(", ")
            }
        );
        println!(
            "Default members: {}",
            if role.default_members.is_empty() {
                "-".to_string()
            } else {
                role.default_members.join(", ")
            }
        );
    }

    Ok(())
}

async fn create_role(args: RoleCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::CreateRoleRequest {
        name: args.name.clone(),
        policies: if args.policy.is_empty() {
            None
        } else {
            Some(args.policy)
        },
        members: if args.member.is_empty() {
            None
        } else {
            Some(args.member)
        },
        default_members: if args.default_member.is_empty() {
            None
        } else {
            Some(args.default_member)
        },
    };

    let response = client
        .inner()
        .create_role()
        .account(account)
        .body(request)
        .send()
        .await?;

    let role = response.into_inner();
    println!("Created role '{}' ({})", role.name, role.id);

    if use_json {
        json::print_json(&role)?;
    }

    Ok(())
}

async fn update_role(args: RoleUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::UpdateRoleRequest {
        name: args.name,
        policies: if args.policy.is_empty() {
            None
        } else {
            Some(args.policy)
        },
        members: if args.member.is_empty() {
            None
        } else {
            Some(args.member)
        },
        default_members: if args.default_member.is_empty() {
            None
        } else {
            Some(args.default_member)
        },
    };

    let response = client
        .inner()
        .update_role()
        .account(account)
        .role(&args.role)
        .body(request)
        .send()
        .await?;

    let role = response.into_inner();
    println!("Updated role '{}'", role.name);

    if use_json {
        json::print_json(&role)?;
    }

    Ok(())
}

pub async fn delete_roles(args: RoleDeleteArgs, client: &TypedClient) -> Result<()> {
    for role_ref in &args.roles {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete role '{}'?", role_ref))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        let account = &client.auth_config().account;

        client
            .inner()
            .delete_role()
            .account(account)
            .role(role_ref)
            .send()
            .await?;

        println!("Deleted role '{}'", role_ref);
    }

    Ok(())
}

/// Add role from file (legacy -a flag support)
///
/// Reads role JSON from:
/// - A file path
/// - stdin (when file is "-")
/// - Interactive prompts (when file is None)
async fn add_role_from_file(
    file: Option<&str>,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    use std::io::{self, Read};

    // Read JSON input based on source
    let json_data: serde_json::Value = match file {
        Some("-") => {
            // Read from stdin
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer)?;
            serde_json::from_str(&buffer)
                .map_err(|e| anyhow::anyhow!("invalid JSON on stdin: {}", e))?
        }
        Some(path) => {
            // Read from file
            let content = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("failed to read file '{}': {}", path, e))?;
            serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("invalid JSON in '{}': {}", path, e))?
        }
        None => {
            // Interactive mode - prompt for fields
            use dialoguer::Input;

            let name: String = Input::new().with_prompt("Name").interact_text()?;

            let policies_str: String = Input::new()
                .with_prompt("Policies (comma-separated, optional)")
                .allow_empty(true)
                .interact_text()?;

            let members_str: String = Input::new()
                .with_prompt("Members (comma-separated user logins, optional)")
                .allow_empty(true)
                .interact_text()?;

            let default_members_str: String = Input::new()
                .with_prompt("Default members (comma-separated user logins, optional)")
                .allow_empty(true)
                .interact_text()?;

            let policies: Vec<String> = if policies_str.is_empty() {
                vec![]
            } else {
                policies_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect()
            };

            let members: Vec<String> = if members_str.is_empty() {
                vec![]
            } else {
                members_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect()
            };

            let default_members: Vec<String> = if default_members_str.is_empty() {
                vec![]
            } else {
                default_members_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect()
            };

            serde_json::json!({
                "name": name,
                "policies": policies,
                "members": members,
                "default_members": default_members,
            })
        }
    };

    // Extract required fields
    let name = json_data
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required field: name"))?
        .to_string();

    // Extract optional arrays (support both naming conventions)
    let policies: Option<Vec<String>> = json_data
        .get("policies")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty());

    let members: Option<Vec<String>> = json_data
        .get("members")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty());

    let default_members: Option<Vec<String>> = json_data
        .get("default_members")
        .or_else(|| json_data.get("defaultMembers"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty());

    // Create the role
    let account = &client.auth_config().account;
    let request = cloudapi_client::types::CreateRoleRequest {
        name: name.clone(),
        policies,
        members,
        default_members,
    };

    let response = client
        .inner()
        .create_role()
        .account(account)
        .body(request)
        .send()
        .await?;

    let role = response.into_inner();
    println!("Created role '{}' ({})", role.name, role.id);

    if use_json {
        json::print_json(&role)?;
    }

    Ok(())
}

/// Struct for deserializing edited role YAML (comments are ignored by serde_yaml)
#[derive(Deserialize)]
struct RoleEdit {
    /// Role name
    name: String,
    /// Users assigned to this role
    #[serde(default)]
    members: Vec<String>,
    /// Policies attached to this role
    #[serde(default)]
    policies: Vec<String>,
    /// Default members (automatically assigned)
    #[serde(default)]
    default_members: Vec<String>,
}

/// Convert a Role to commented YAML for editing
fn role_to_commented_yaml(role: &cloudapi_client::types::Role, account: &str) -> String {
    let members = editor::format_yaml_list(&role.members, "  ");
    let policies = editor::format_yaml_list(&role.policies, "  ");
    let default_members = editor::format_yaml_list(&role.default_members, "  ");

    format!(
        r#"# Role: {name}
# ID: {id}
# Account: {account}
# Edit below, save and quit to apply changes

# Role name (required)
name: {name}

# Users assigned to this role
members:
{members}

# Policies attached to this role
policies:
{policies}

# Default members (automatically assigned to new users)
default_members:
{default_members}
"#,
        name = role.name,
        id = role.id,
        account = account,
        members = members,
        policies = policies,
        default_members = default_members,
    )
}

/// Edit role in $EDITOR (legacy -e flag support)
async fn edit_role_in_editor(role_ref: &str, client: &TypedClient) -> Result<()> {
    let account = client.auth_config().account.clone();

    // Fetch current role
    let response = client
        .inner()
        .get_role()
        .account(&account)
        .role(role_ref)
        .send()
        .await?;
    let role = response.into_inner();

    let filename = format!("{}-role-{}.yaml", account, role.name);
    let original_yaml = role_to_commented_yaml(&role, &account);

    let mut current_yaml = original_yaml.clone();
    loop {
        let result = editor::edit_in_editor(&current_yaml, &filename)?;

        if !result.changed {
            println!("No changes made");
            return Ok(());
        }

        match serde_yaml::from_str::<RoleEdit>(&result.content) {
            Ok(edited) => {
                // Build update request
                let request = cloudapi_client::types::UpdateRoleRequest {
                    name: Some(edited.name.clone()),
                    members: if edited.members.is_empty() {
                        None
                    } else {
                        Some(edited.members)
                    },
                    policies: if edited.policies.is_empty() {
                        None
                    } else {
                        Some(edited.policies)
                    },
                    default_members: if edited.default_members.is_empty() {
                        None
                    } else {
                        Some(edited.default_members)
                    },
                };

                // Update the role
                client
                    .inner()
                    .update_role()
                    .account(&account)
                    .role(&role.name)
                    .body(request)
                    .send()
                    .await?;

                println!("Updated role \"{}\"", edited.name);
                return Ok(());
            }
            Err(e) => {
                eprintln!("Error parsing YAML: {}", e);
                if !editor::prompt_retry()? {
                    anyhow::bail!("Aborted");
                }
                current_yaml = result.content; // Keep user's edits for retry
            }
        }
    }
}
