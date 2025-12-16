// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC apply and reset commands

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::output::{json, table};

use super::common::resolve_user;

#[derive(Args, Clone)]
pub struct ApplyArgs {
    /// Path to RBAC configuration file (JSON format)
    pub file: PathBuf,
    /// Show what would be done without making changes
    #[arg(long, short = 'n')]
    pub dry_run: bool,
    /// Skip confirmation prompts
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
    /// Generate SSH keys and CLI profiles for each user (development/testing only)
    #[arg(long, hide = true)]
    pub dev_create_keys_and_profiles: bool,
}

#[derive(Args, Clone)]
pub struct ResetArgs {
    /// Skip confirmation prompt
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

/// RBAC configuration file format
#[derive(Debug, Deserialize)]
struct RbacConfig {
    #[serde(default)]
    users: Vec<RbacConfigUser>,
    #[serde(default)]
    roles: Vec<RbacConfigRole>,
    #[serde(default)]
    policies: Vec<RbacConfigPolicy>,
}

#[derive(Debug, Deserialize)]
struct RbacConfigUser {
    login: String,
    email: String,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    company_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RbacConfigRole {
    name: String,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    default_members: Vec<String>,
    #[serde(default)]
    policies: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RbacConfigPolicy {
    name: String,
    #[serde(default)]
    description: Option<String>,
    rules: Vec<String>,
}

/// A change to be applied to RBAC configuration
#[derive(Debug)]
enum RbacChange {
    CreateUser {
        login: String,
        email: String,
        first_name: Option<String>,
        last_name: Option<String>,
        company_name: Option<String>,
    },
    UpdateUser {
        login: String,
        email: Option<String>,
        first_name: Option<String>,
        last_name: Option<String>,
        company_name: Option<String>,
    },
    DeleteUser {
        login: String,
    },
    CreatePolicy {
        name: String,
        description: Option<String>,
        rules: Vec<String>,
    },
    UpdatePolicy {
        name: String,
        description: Option<String>,
        rules: Option<Vec<String>>,
    },
    DeletePolicy {
        name: String,
    },
    CreateRole {
        name: String,
        members: Vec<String>,
        default_members: Vec<String>,
        policies: Vec<String>,
    },
    UpdateRole {
        name: String,
        members: Option<Vec<String>>,
        default_members: Option<Vec<String>>,
        policies: Option<Vec<String>>,
    },
    DeleteRole {
        name: String,
    },
}

impl std::fmt::Display for RbacChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RbacChange::CreateUser { login, .. } => write!(f, "Create user '{}'", login),
            RbacChange::UpdateUser { login, .. } => write!(f, "Update user '{}'", login),
            RbacChange::DeleteUser { login } => write!(f, "Delete user '{}'", login),
            RbacChange::CreatePolicy { name, .. } => write!(f, "Create policy '{}'", name),
            RbacChange::UpdatePolicy { name, .. } => write!(f, "Update policy '{}'", name),
            RbacChange::DeletePolicy { name } => write!(f, "Delete policy '{}'", name),
            RbacChange::CreateRole { name, .. } => write!(f, "Create role '{}'", name),
            RbacChange::UpdateRole { name, .. } => write!(f, "Update role '{}'", name),
            RbacChange::DeleteRole { name } => write!(f, "Delete role '{}'", name),
        }
    }
}

/// Result of applying RBAC configuration
#[derive(serde::Serialize)]
struct ApplyResult {
    changes: Vec<ApplyChangeResult>,
    summary: ApplySummary,
}

#[derive(serde::Serialize)]
struct ApplyChangeResult {
    action: String,
    #[serde(rename = "type")]
    item_type: String,
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct ApplySummary {
    users_created: usize,
    users_updated: usize,
    users_deleted: usize,
    policies_created: usize,
    policies_updated: usize,
    policies_deleted: usize,
    roles_created: usize,
    roles_updated: usize,
    roles_deleted: usize,
}

/// RBAC info JSON output structure
#[derive(serde::Serialize)]
pub struct RbacInfo {
    users: Vec<cloudapi_client::types::User>,
    roles: Vec<cloudapi_client::types::Role>,
    policies: Vec<cloudapi_client::types::Policy>,
}

pub async fn rbac_info(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // Fetch all RBAC data concurrently
    let (users_result, roles_result, policies_result) = tokio::join!(
        client.inner().list_users().account(account).send(),
        client.inner().list_roles().account(account).send(),
        client.inner().list_policies().account(account).send(),
    );

    let users = users_result?.into_inner();
    let roles = roles_result?.into_inner();
    let policies = policies_result?.into_inner();

    if use_json {
        let info = RbacInfo {
            users,
            roles,
            policies,
        };
        json::print_json(&info)?;
    } else {
        // Summary section
        println!("RBAC Summary");
        println!("============");
        println!("Users:    {}", users.len());
        println!("Roles:    {}", roles.len());
        println!("Policies: {}", policies.len());
        println!();

        // Users section
        if !users.is_empty() {
            println!("Users:");
            let mut tbl = table::create_table(&["SHORTID", "LOGIN", "EMAIL"]);
            for user in &users {
                tbl.add_row(vec![&user.id.to_string()[..8], &user.login, &user.email]);
            }
            table::print_table(tbl);
            println!();
        }

        // Roles section
        if !roles.is_empty() {
            println!("Roles:");
            let mut tbl = table::create_table(&["SHORTID", "NAME", "POLICIES", "MEMBERS"]);
            for role in &roles {
                let policies_str = if role.policies.is_empty() {
                    "-".to_string()
                } else {
                    role.policies.join(", ")
                };
                let members_str = if role.members.is_empty() {
                    "-".to_string()
                } else {
                    role.members.join(", ")
                };
                tbl.add_row(vec![
                    &role.id.to_string()[..8],
                    &role.name,
                    &policies_str,
                    &members_str,
                ]);
            }
            table::print_table(tbl);
            println!();
        }

        // Policies section
        if !policies.is_empty() {
            println!("Policies:");
            let mut tbl = table::create_table(&["SHORTID", "NAME", "RULES"]);
            for policy in &policies {
                tbl.add_row(vec![
                    &policy.id.to_string()[..8],
                    &policy.name,
                    &format!("{} rule(s)", policy.rules.len()),
                ]);
            }
            table::print_table(tbl);
        }
    }

    Ok(())
}

pub async fn rbac_apply(args: ApplyArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    // Check for unimplemented dev option
    if args.dev_create_keys_and_profiles {
        return Err(anyhow::anyhow!(
            "The --dev-create-keys-and-profiles option is not yet implemented.\n\
             This development option would automatically generate SSH keys and \n\
             CLI profiles for each RBAC user. For now, please create keys and \n\
             profiles manually."
        ));
    }

    // Read and parse the config file
    let content = std::fs::read_to_string(&args.file).map_err(|e| {
        anyhow::anyhow!(
            "Failed to read config file '{}': {}",
            args.file.display(),
            e
        )
    })?;

    let config: RbacConfig = serde_json::from_str(&content).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse config file '{}': {}",
            args.file.display(),
            e
        )
    })?;

    let account = &client.auth_config().account;

    // Fetch current state
    let (users_result, roles_result, policies_result) = tokio::join!(
        client.inner().list_users().account(account).send(),
        client.inner().list_roles().account(account).send(),
        client.inner().list_policies().account(account).send(),
    );

    let current_users = users_result?.into_inner();
    let current_roles = roles_result?.into_inner();
    let current_policies = policies_result?.into_inner();

    // Build maps for quick lookup
    let current_user_map: HashMap<String, _> =
        current_users.iter().map(|u| (u.login.clone(), u)).collect();
    let current_role_map: HashMap<String, _> =
        current_roles.iter().map(|r| (r.name.clone(), r)).collect();
    let current_policy_map: HashMap<String, _> = current_policies
        .iter()
        .map(|p| (p.name.clone(), p))
        .collect();

    // Build desired state sets
    let want_users: HashSet<String> = config.users.iter().map(|u| u.login.clone()).collect();
    let want_roles: HashSet<String> = config.roles.iter().map(|r| r.name.clone()).collect();
    let want_policies: HashSet<String> = config.policies.iter().map(|p| p.name.clone()).collect();

    // Calculate changes
    // Order: create policies first, then users, then roles (roles reference users and policies)
    // Delete order: roles first, then users, then policies
    let mut changes = Vec::new();

    // Policy changes
    for policy in &config.policies {
        if let Some(current) = current_policy_map.get(&policy.name) {
            // Check if update needed
            let rules_differ = {
                let mut current_rules: Vec<_> = current.rules.clone();
                let mut want_rules: Vec<_> = policy.rules.clone();
                current_rules.sort();
                want_rules.sort();
                current_rules != want_rules
            };
            let desc_differs = policy.description != current.description;

            if rules_differ || desc_differs {
                changes.push(RbacChange::UpdatePolicy {
                    name: policy.name.clone(),
                    description: if desc_differs {
                        policy.description.clone()
                    } else {
                        None
                    },
                    rules: if rules_differ {
                        Some(policy.rules.clone())
                    } else {
                        None
                    },
                });
            }
        } else {
            changes.push(RbacChange::CreatePolicy {
                name: policy.name.clone(),
                description: policy.description.clone(),
                rules: policy.rules.clone(),
            });
        }
    }

    // User changes
    for user in &config.users {
        if let Some(current) = current_user_map.get(&user.login) {
            // Check if update needed
            let email_differs = current.email != user.email;
            let fn_differs = current.first_name != user.first_name;
            let ln_differs = current.last_name != user.last_name;
            let cn_differs = current.company_name != user.company_name;

            if email_differs || fn_differs || ln_differs || cn_differs {
                changes.push(RbacChange::UpdateUser {
                    login: user.login.clone(),
                    email: if email_differs {
                        Some(user.email.clone())
                    } else {
                        None
                    },
                    first_name: if fn_differs {
                        user.first_name.clone()
                    } else {
                        None
                    },
                    last_name: if ln_differs {
                        user.last_name.clone()
                    } else {
                        None
                    },
                    company_name: if cn_differs {
                        user.company_name.clone()
                    } else {
                        None
                    },
                });
            }
        } else {
            changes.push(RbacChange::CreateUser {
                login: user.login.clone(),
                email: user.email.clone(),
                first_name: user.first_name.clone(),
                last_name: user.last_name.clone(),
                company_name: user.company_name.clone(),
            });
        }
    }

    // Role changes
    for role in &config.roles {
        if let Some(current) = current_role_map.get(&role.name) {
            // Check if update needed
            let members_differ = {
                let mut cm: Vec<_> = current.members.clone();
                let mut wm: Vec<_> = role.members.clone();
                cm.sort();
                wm.sort();
                cm != wm
            };
            let default_members_differ = {
                let mut cdm: Vec<_> = current.default_members.clone();
                let mut wdm: Vec<_> = role.default_members.clone();
                cdm.sort();
                wdm.sort();
                cdm != wdm
            };
            let policies_differ = {
                let mut cp: Vec<_> = current.policies.clone();
                let mut wp: Vec<_> = role.policies.clone();
                cp.sort();
                wp.sort();
                cp != wp
            };

            if members_differ || default_members_differ || policies_differ {
                changes.push(RbacChange::UpdateRole {
                    name: role.name.clone(),
                    members: if members_differ {
                        Some(role.members.clone())
                    } else {
                        None
                    },
                    default_members: if default_members_differ {
                        Some(role.default_members.clone())
                    } else {
                        None
                    },
                    policies: if policies_differ {
                        Some(role.policies.clone())
                    } else {
                        None
                    },
                });
            }
        } else {
            changes.push(RbacChange::CreateRole {
                name: role.name.clone(),
                members: role.members.clone(),
                default_members: role.default_members.clone(),
                policies: role.policies.clone(),
            });
        }
    }

    // Deletions (roles first, then users, then policies)
    for role in &current_roles {
        if !want_roles.contains(&role.name) {
            changes.push(RbacChange::DeleteRole {
                name: role.name.clone(),
            });
        }
    }
    for user in &current_users {
        if !want_users.contains(&user.login) {
            changes.push(RbacChange::DeleteUser {
                login: user.login.clone(),
            });
        }
    }
    for policy in &current_policies {
        if !want_policies.contains(&policy.name) {
            changes.push(RbacChange::DeletePolicy {
                name: policy.name.clone(),
            });
        }
    }

    // Sort changes: creates first (policies, users, roles), then updates, then deletes (roles, users, policies)
    changes.sort_by_key(|c| match c {
        RbacChange::CreatePolicy { .. } => 0,
        RbacChange::CreateUser { .. } => 1,
        RbacChange::CreateRole { .. } => 2,
        RbacChange::UpdatePolicy { .. } => 3,
        RbacChange::UpdateUser { .. } => 4,
        RbacChange::UpdateRole { .. } => 5,
        RbacChange::DeleteRole { .. } => 6,
        RbacChange::DeleteUser { .. } => 7,
        RbacChange::DeletePolicy { .. } => 8,
    });

    if changes.is_empty() {
        if use_json {
            json::print_json(&ApplyResult {
                changes: vec![],
                summary: ApplySummary {
                    users_created: 0,
                    users_updated: 0,
                    users_deleted: 0,
                    policies_created: 0,
                    policies_updated: 0,
                    policies_deleted: 0,
                    roles_created: 0,
                    roles_updated: 0,
                    roles_deleted: 0,
                },
            })?;
        } else {
            println!("No changes required. RBAC configuration is up to date.");
        }
        return Ok(());
    }

    // Show planned changes
    if !use_json {
        println!("Planned changes:");
        for change in &changes {
            println!("  - {}", change);
        }
        println!();
    }

    // Dry run mode
    if args.dry_run {
        if use_json {
            let change_results: Vec<_> = changes
                .iter()
                .map(|c| {
                    let (action, item_type, name) = match c {
                        RbacChange::CreateUser { login, .. } => ("create", "user", login.clone()),
                        RbacChange::UpdateUser { login, .. } => ("update", "user", login.clone()),
                        RbacChange::DeleteUser { login } => ("delete", "user", login.clone()),
                        RbacChange::CreatePolicy { name, .. } => ("create", "policy", name.clone()),
                        RbacChange::UpdatePolicy { name, .. } => ("update", "policy", name.clone()),
                        RbacChange::DeletePolicy { name } => ("delete", "policy", name.clone()),
                        RbacChange::CreateRole { name, .. } => ("create", "role", name.clone()),
                        RbacChange::UpdateRole { name, .. } => ("update", "role", name.clone()),
                        RbacChange::DeleteRole { name } => ("delete", "role", name.clone()),
                    };
                    ApplyChangeResult {
                        action: action.to_string(),
                        item_type: item_type.to_string(),
                        name,
                        status: "dry-run".to_string(),
                        error: None,
                    }
                })
                .collect();
            json::print_json(&serde_json::json!({
                "dry_run": true,
                "changes": change_results,
            }))?;
        } else {
            println!("[dry-run] {} change(s) would be applied.", changes.len());
        }
        return Ok(());
    }

    // Confirm if not forced
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt(format!("Apply {} change(s)?", changes.len()))
            .default(false)
            .interact()?
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Execute changes
    let mut summary = ApplySummary {
        users_created: 0,
        users_updated: 0,
        users_deleted: 0,
        policies_created: 0,
        policies_updated: 0,
        policies_deleted: 0,
        roles_created: 0,
        roles_updated: 0,
        roles_deleted: 0,
    };
    let mut results = Vec::new();

    for change in &changes {
        let result = execute_rbac_change(change, client).await;

        let (action, item_type, name) = match change {
            RbacChange::CreateUser { login, .. } => ("create", "user", login.clone()),
            RbacChange::UpdateUser { login, .. } => ("update", "user", login.clone()),
            RbacChange::DeleteUser { login } => ("delete", "user", login.clone()),
            RbacChange::CreatePolicy { name, .. } => ("create", "policy", name.clone()),
            RbacChange::UpdatePolicy { name, .. } => ("update", "policy", name.clone()),
            RbacChange::DeletePolicy { name } => ("delete", "policy", name.clone()),
            RbacChange::CreateRole { name, .. } => ("create", "role", name.clone()),
            RbacChange::UpdateRole { name, .. } => ("update", "role", name.clone()),
            RbacChange::DeleteRole { name } => ("delete", "role", name.clone()),
        };

        match &result {
            Ok(()) => {
                if !use_json {
                    println!("  {} {}", action, name);
                }
                match change {
                    RbacChange::CreateUser { .. } => summary.users_created += 1,
                    RbacChange::UpdateUser { .. } => summary.users_updated += 1,
                    RbacChange::DeleteUser { .. } => summary.users_deleted += 1,
                    RbacChange::CreatePolicy { .. } => summary.policies_created += 1,
                    RbacChange::UpdatePolicy { .. } => summary.policies_updated += 1,
                    RbacChange::DeletePolicy { .. } => summary.policies_deleted += 1,
                    RbacChange::CreateRole { .. } => summary.roles_created += 1,
                    RbacChange::UpdateRole { .. } => summary.roles_updated += 1,
                    RbacChange::DeleteRole { .. } => summary.roles_deleted += 1,
                }
                results.push(ApplyChangeResult {
                    action: action.to_string(),
                    item_type: item_type.to_string(),
                    name,
                    status: "success".to_string(),
                    error: None,
                });
            }
            Err(e) => {
                if !use_json {
                    println!("  {} {} - FAILED: {}", action, name, e);
                }
                results.push(ApplyChangeResult {
                    action: action.to_string(),
                    item_type: item_type.to_string(),
                    name,
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                });
            }
        }
    }

    if use_json {
        json::print_json(&ApplyResult {
            changes: results,
            summary,
        })?;
    } else {
        println!();
        println!("Summary:");
        if summary.users_created > 0 || summary.users_updated > 0 || summary.users_deleted > 0 {
            println!(
                "  Users: {} created, {} updated, {} deleted",
                summary.users_created, summary.users_updated, summary.users_deleted
            );
        }
        if summary.policies_created > 0
            || summary.policies_updated > 0
            || summary.policies_deleted > 0
        {
            println!(
                "  Policies: {} created, {} updated, {} deleted",
                summary.policies_created, summary.policies_updated, summary.policies_deleted
            );
        }
        if summary.roles_created > 0 || summary.roles_updated > 0 || summary.roles_deleted > 0 {
            println!(
                "  Roles: {} created, {} updated, {} deleted",
                summary.roles_created, summary.roles_updated, summary.roles_deleted
            );
        }
    }

    Ok(())
}

/// Generate a random password for new users
fn generate_password() -> String {
    use std::fmt::Write;
    let mut rng = [0u8; 24];
    // Use a simple random source
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // Mix in some entropy from time
    let seed = now.as_nanos();
    for (i, b) in rng.iter_mut().enumerate() {
        *b = ((seed >> (i * 3)) & 0xFF) as u8 ^ (i as u8 * 17);
    }
    let mut result = String::with_capacity(32);
    for b in &rng {
        write!(result, "{:02x}", b).unwrap();
    }
    result
}

async fn execute_rbac_change(change: &RbacChange, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    match change {
        RbacChange::CreateUser {
            login,
            email,
            first_name,
            last_name,
            company_name,
        } => {
            let request = cloudapi_client::types::CreateUserRequest {
                login: login.clone(),
                email: email.clone(),
                password: generate_password(),
                company_name: company_name.clone(),
                first_name: first_name.clone(),
                last_name: last_name.clone(),
                phone: None,
            };
            client
                .inner()
                .create_user()
                .account(account)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::UpdateUser {
            login,
            email,
            first_name,
            last_name,
            company_name,
        } => {
            let user_id = resolve_user(login, client).await?;
            let request = cloudapi_client::types::UpdateUserRequest {
                email: email.clone(),
                company_name: company_name.clone(),
                first_name: first_name.clone(),
                last_name: last_name.clone(),
                phone: None,
            };
            client
                .inner()
                .update_user()
                .account(account)
                .uuid(&user_id)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::DeleteUser { login } => {
            let user_id = resolve_user(login, client).await?;
            client
                .inner()
                .delete_user()
                .account(account)
                .uuid(&user_id)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::CreatePolicy {
            name,
            description,
            rules,
        } => {
            let request = cloudapi_client::types::CreatePolicyRequest {
                name: name.clone(),
                rules: rules.clone(),
                description: description.clone(),
            };
            client
                .inner()
                .create_policy()
                .account(account)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::UpdatePolicy {
            name,
            description,
            rules,
        } => {
            let request = cloudapi_client::types::UpdatePolicyRequest {
                name: None,
                rules: rules.clone(),
                description: description.clone(),
            };
            client
                .inner()
                .update_policy()
                .account(account)
                .policy(name)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::DeletePolicy { name } => {
            client
                .inner()
                .delete_policy()
                .account(account)
                .policy(name)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::CreateRole {
            name,
            members,
            default_members,
            policies,
        } => {
            let request = cloudapi_client::types::CreateRoleRequest {
                name: name.clone(),
                policies: if policies.is_empty() {
                    None
                } else {
                    Some(policies.clone())
                },
                members: if members.is_empty() {
                    None
                } else {
                    Some(members.clone())
                },
                default_members: if default_members.is_empty() {
                    None
                } else {
                    Some(default_members.clone())
                },
            };
            client
                .inner()
                .create_role()
                .account(account)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::UpdateRole {
            name,
            members,
            default_members,
            policies,
        } => {
            let request = cloudapi_client::types::UpdateRoleRequest {
                name: None,
                policies: policies.clone(),
                members: members.clone(),
                default_members: default_members.clone(),
            };
            client
                .inner()
                .update_role()
                .account(account)
                .role(name)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::DeleteRole { name } => {
            client
                .inner()
                .delete_role()
                .account(account)
                .role(name)
                .send()
                .await?;
            Ok(())
        }
    }
}

pub async fn rbac_reset(args: ResetArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    // Fetch current state
    let (users_result, roles_result, policies_result) = tokio::join!(
        client.inner().list_users().account(account).send(),
        client.inner().list_roles().account(account).send(),
        client.inner().list_policies().account(account).send(),
    );

    let users = users_result?.into_inner();
    let roles = roles_result?.into_inner();
    let policies = policies_result?.into_inner();

    let total = users.len() + roles.len() + policies.len();

    if total == 0 {
        println!("No RBAC configuration to reset.");
        return Ok(());
    }

    println!("This will delete:");
    if !users.is_empty() {
        println!(
            "  - {} user(s): {}",
            users.len(),
            users
                .iter()
                .map(|u| u.login.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !roles.is_empty() {
        println!(
            "  - {} role(s): {}",
            roles.len(),
            roles
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !policies.is_empty() {
        println!(
            "  - {} policy(ies): {}",
            policies.len(),
            policies
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!();

    // Confirm if not forced
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt("Are you sure you want to delete all RBAC configuration?")
            .default(false)
            .interact()?
        {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Delete in order: roles first (they reference users/policies), then users, then policies
    let mut errors = Vec::new();

    // Delete roles
    for role in &roles {
        if let Err(e) = client
            .inner()
            .delete_role()
            .account(account)
            .role(&role.name)
            .send()
            .await
        {
            errors.push(format!("Failed to delete role '{}': {}", role.name, e));
        } else {
            println!("Deleted role '{}'", role.name);
        }
    }

    // Delete users
    for user in &users {
        if let Err(e) = client
            .inner()
            .delete_user()
            .account(account)
            .uuid(user.id.to_string())
            .send()
            .await
        {
            errors.push(format!("Failed to delete user '{}': {}", user.login, e));
        } else {
            println!("Deleted user '{}'", user.login);
        }
    }

    // Delete policies
    for policy in &policies {
        if let Err(e) = client
            .inner()
            .delete_policy()
            .account(account)
            .policy(&policy.name)
            .send()
            .await
        {
            errors.push(format!("Failed to delete policy '{}': {}", policy.name, e));
        } else {
            println!("Deleted policy '{}'", policy.name);
        }
    }

    if !errors.is_empty() {
        println!();
        println!("Errors occurred:");
        for err in &errors {
            println!("  - {}", err);
        }
        return Err(anyhow::anyhow!(
            "{} error(s) occurred during reset",
            errors.len()
        ));
    }

    println!();
    println!("RBAC configuration reset complete.");

    Ok(())
}
