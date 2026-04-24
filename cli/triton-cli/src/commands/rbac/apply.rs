// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! RBAC apply and reset commands

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::config::{Config, Profile, paths};
use crate::output::{json, table};

use super::common::resolve_user;

#[derive(Args, Clone)]
pub struct InfoArgs {
    /// Include all info for a more full report (includes SSH keys per user)
    #[arg(short = 'a', long = "all")]
    pub all: bool,
    /// Do not color the output with ANSI codes
    #[arg(long = "no-color")]
    pub no_color: bool,
}

#[derive(Args, Clone)]
pub struct ApplyArgs {
    /// Path to RBAC configuration file (JSON format, default: ./rbac.json)
    #[arg(short = 'f', long = "file", default_value = "./rbac.json")]
    pub file: PathBuf,
    /// Show what would be done without making changes
    #[arg(long, short = 'n')]
    pub dry_run: bool,
    /// Skip confirmation prompts
    #[arg(long, short = 'y', visible_alias = "yes")]
    pub force: bool,
    /// Generate SSH keys and CLI profiles for each user (development/testing only)
    #[arg(long, hide = true)]
    pub dev_create_keys_and_profiles: bool,
    /// SSH key type for dev key generation (ed25519 or rsa)
    #[arg(long, default_value = "ed25519", hide = true)]
    pub dev_key_type: DevKeyType,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum DevKeyType {
    Ed25519,
    Rsa,
}

#[derive(Args, Clone)]
pub struct ResetArgs {
    /// Skip confirmation prompt
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
    /// Show what would be deleted without making changes
    #[arg(long, short = 'n')]
    pub dry_run: bool,
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
    #[serde(default)]
    keys: Option<String>,
}

/// A parsed SSH public key from a config-specified key file
#[derive(Debug)]
struct ParsedKey {
    name: String,
    key: String,
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
    CreateKey {
        user_login: String,
        key_name: String,
        key_material: String,
    },
    DeleteKey {
        user_login: String,
        key_name: String,
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
            RbacChange::CreateKey {
                user_login,
                key_name,
                ..
            } => write!(f, "Create key '{}' for user '{}'", key_name, user_login),
            RbacChange::DeleteKey {
                user_login,
                key_name,
            } => write!(f, "Delete key '{}' for user '{}'", key_name, user_login),
        }
    }
}

/// Result of applying RBAC configuration
#[derive(serde::Serialize)]
struct ApplyResult {
    changes: Vec<ApplyChangeResult>,
    summary: ApplySummary,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum ApplyStatus {
    Success,
    Failed,
    DryRun,
}

#[derive(serde::Serialize)]
struct ApplyChangeResult {
    action: String,
    #[serde(rename = "type")]
    item_type: String,
    name: String,
    status: ApplyStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct ApplySummary {
    users_created: usize,
    users_updated: usize,
    users_deleted: usize,
    keys_created: usize,
    keys_deleted: usize,
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

pub async fn rbac_info(args: InfoArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();

    // Fetch all RBAC data concurrently
    let (users_result, roles_result, policies_result) = tokio::join!(
        client.inner().list_users().account(account).send(),
        client.inner().list_roles().account(account).send(),
        client.inner().list_policies().account(account).send(),
    );

    let users = users_result?.into_inner();
    let roles = roles_result?.into_inner();
    let policies = policies_result?.into_inner();

    // If --all flag is set, fetch keys for each user
    let mut key_fetch_errors: Vec<String> = Vec::new();
    let user_keys: HashMap<String, Vec<cloudapi_client::types::SshKey>> = if args.all {
        let mut keys_map = HashMap::new();
        for user in &users {
            let keys_result = client
                .inner()
                .list_user_keys()
                .account(account)
                .uuid(user.id.to_string())
                .send()
                .await;
            match keys_result {
                Ok(keys) => {
                    keys_map.insert(user.id.to_string(), keys.into_inner());
                }
                Err(e) => {
                    eprintln!(
                        "Warning: failed to fetch keys for user {}: {}",
                        user.login, e
                    );
                    key_fetch_errors.push(user.id.to_string());
                }
            }
        }
        keys_map
    } else {
        HashMap::new()
    };

    // Helper for ANSI styling (respects --no-color)
    use std::io::IsTerminal;
    let use_color = !args.no_color && std::io::stdout().is_terminal();
    let stylize = |s: &str, style: &str| -> String {
        if !use_color {
            s.to_string()
        } else {
            match style {
                "bold" => format!("\x1b[1m{}\x1b[0m", s),
                "red" => format!("\x1b[31m{}\x1b[0m", s),
                _ => s.to_string(),
            }
        }
    };

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
            let headers = if args.all {
                vec!["SHORTID", "LOGIN", "EMAIL", "KEYS"]
            } else {
                vec!["SHORTID", "LOGIN", "EMAIL"]
            };
            let mut tbl = table::create_table(&headers);
            for user in &users {
                let user_id_str = user.id.to_string();
                let short_id = &user_id_str[..8];
                if args.all {
                    let keys_str = if key_fetch_errors.contains(&user_id_str) {
                        stylize("error", "red")
                    } else {
                        let keys_count = user_keys.get(&user_id_str).map(|k| k.len()).unwrap_or(0);
                        if keys_count == 0 {
                            stylize("no keys", "red")
                        } else {
                            format!("{} key(s)", keys_count)
                        }
                    };
                    tbl.add_row(vec![short_id, &user.login, &user.email, &keys_str]);
                } else {
                    tbl.add_row(vec![short_id, &user.login, &user.email]);
                }
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
                    stylize("no policies", "red")
                } else {
                    role.policies
                        .iter()
                        .filter_map(|p| p.name.as_deref())
                        .collect::<Vec<_>>()
                        .join(", ")
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
                let rules_str = if policy.rules.is_empty() {
                    stylize("no rules", "red")
                } else {
                    format!("{} rule(s)", policy.rules.len())
                };
                tbl.add_row(vec![&policy.id.to_string()[..8], &policy.name, &rules_str]);
            }
            table::print_table(tbl);
        }
    }

    Ok(())
}

pub async fn rbac_apply(args: ApplyArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    // Resolve the current profile for dev mode (if enabled)
    let base_profile = if args.dev_create_keys_and_profiles {
        let profile_name = Config::load()
            .await
            .map_err(|e| anyhow::anyhow!("failed to load config: {e}"))?
            .profile
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--dev-create-keys-and-profiles requires a configured profile.\n\
                     Use 'triton profile create' to create one first."
                )
            })?;
        Some(Profile::load(&profile_name).await?)
    } else {
        None
    };

    // Read and parse the config file
    let content = tokio::fs::read_to_string(&args.file).await.map_err(|e| {
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

    let account = client.effective_account();

    // Fetch current state
    let (users_result, roles_result, policies_result) = tokio::join!(
        client.inner().list_users().account(account).send(),
        client.inner().list_roles().account(account).send(),
        client.inner().list_policies().account(account).send(),
    );

    let current_users = users_result?.into_inner();
    let current_roles = roles_result?.into_inner();
    let current_policies = policies_result?.into_inner();

    // Fetch current SSH keys for each user
    let mut current_user_keys: HashMap<String, Vec<cloudapi_client::types::SshKey>> =
        HashMap::new();
    for user in &current_users {
        if let Ok(keys) = client
            .inner()
            .list_user_keys()
            .account(account)
            .uuid(user.id.to_string())
            .send()
            .await
        {
            current_user_keys.insert(user.login.clone(), keys.into_inner());
        }
    }

    // Load desired keys from config files
    let config_dir = args.file.parent().unwrap_or(Path::new("."));
    let mut wanted_user_keys: HashMap<String, Vec<ParsedKey>> = HashMap::new();
    for user in &config.users {
        wanted_user_keys.insert(user.login.clone(), load_user_keys(user, config_dir).await?);
    }

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
                let mut current_rules: Vec<_> = current.rules.0.clone();
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

            // Compare keys for existing user
            let wanted_keys = wanted_user_keys
                .get(&user.login)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let current_keys = current_user_keys
                .get(&user.login)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let current_key_names: HashSet<&str> =
                current_keys.iter().map(|k| k.name.as_str()).collect();
            let wanted_key_names: HashSet<&str> =
                wanted_keys.iter().map(|k| k.name.as_str()).collect();

            // Keys to create: in wanted but not in current
            for key in wanted_keys {
                if !current_key_names.contains(key.name.as_str()) {
                    changes.push(RbacChange::CreateKey {
                        user_login: user.login.clone(),
                        key_name: key.name.clone(),
                        key_material: key.key.clone(),
                    });
                }
            }
            // Keys to delete: in current but not in wanted
            for key in current_keys {
                if !wanted_key_names.contains(key.name.as_str()) {
                    changes.push(RbacChange::DeleteKey {
                        user_login: user.login.clone(),
                        key_name: key.name.clone(),
                    });
                }
            }
        } else {
            changes.push(RbacChange::CreateUser {
                login: user.login.clone(),
                email: user.email.clone(),
                first_name: user.first_name.clone(),
                last_name: user.last_name.clone(),
                company_name: user.company_name.clone(),
            });
            // Add key creates for new user
            if let Some(keys) = wanted_user_keys.get(&user.login) {
                for key in keys {
                    changes.push(RbacChange::CreateKey {
                        user_login: user.login.clone(),
                        key_name: key.name.clone(),
                        key_material: key.key.clone(),
                    });
                }
            }
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
                let mut cp: Vec<String> = current
                    .policies
                    .iter()
                    .filter_map(|p| p.name.clone())
                    .collect();
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
            // Delete user's keys before deleting the user
            if let Some(keys) = current_user_keys.get(&user.login) {
                for key in keys {
                    changes.push(RbacChange::DeleteKey {
                        user_login: user.login.clone(),
                        key_name: key.name.clone(),
                    });
                }
            }
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
        RbacChange::CreateKey { .. } => 2,
        RbacChange::CreateRole { .. } => 3,
        RbacChange::UpdatePolicy { .. } => 4,
        RbacChange::UpdateUser { .. } => 5,
        RbacChange::UpdateRole { .. } => 6,
        RbacChange::DeleteRole { .. } => 7,
        RbacChange::DeleteKey { .. } => 8,
        RbacChange::DeleteUser { .. } => 9,
        RbacChange::DeletePolicy { .. } => 10,
    });

    if changes.is_empty() {
        if use_json {
            json::print_json(&ApplyResult {
                changes: vec![],
                summary: ApplySummary {
                    users_created: 0,
                    users_updated: 0,
                    users_deleted: 0,
                    keys_created: 0,
                    keys_deleted: 0,
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
        // Collect users that would be created for dev mode preview
        let users_to_create: Vec<RbacConfigUser> = changes
            .iter()
            .filter_map(|c| {
                if let RbacChange::CreateUser {
                    login,
                    email,
                    first_name,
                    last_name,
                    company_name,
                } = c
                {
                    Some(RbacConfigUser {
                        login: login.clone(),
                        email: email.clone(),
                        first_name: first_name.clone(),
                        last_name: last_name.clone(),
                        company_name: company_name.clone(),
                        keys: None,
                    })
                } else {
                    None
                }
            })
            .collect();

        if use_json {
            let change_results: Vec<_> = changes
                .iter()
                .map(|c| {
                    let (action, item_type, name) = match c {
                        RbacChange::CreateUser { login, .. } => ("create", "user", login.clone()),
                        RbacChange::UpdateUser { login, .. } => ("update", "user", login.clone()),
                        RbacChange::DeleteUser { login } => ("delete", "user", login.clone()),
                        RbacChange::CreateKey { key_name, .. } => {
                            ("create", "key", key_name.clone())
                        }
                        RbacChange::DeleteKey { key_name, .. } => {
                            ("delete", "key", key_name.clone())
                        }
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
                        status: ApplyStatus::DryRun,
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

        // Show dev mode preview if enabled
        if let Some(profile) = &base_profile {
            if !users_to_create.is_empty() {
                if !use_json {
                    println!();
                    println!(
                        "[dry-run] Dev mode would create keys/profiles for {} user(s):",
                        users_to_create.len()
                    );
                    for user in &users_to_create {
                        println!("  - Generate SSH key for user '{}'", user.login);
                        println!(
                            "  - Upload key '{}-{}' to CloudAPI",
                            profile.name, user.login
                        );
                        println!(
                            "  - Create CLI profile '{}-user-{}'",
                            profile.name, user.login
                        );
                    }
                }
            } else if !use_json {
                println!();
                println!("[dry-run] Dev mode: No new users would be created.");
            }
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
        keys_created: 0,
        keys_deleted: 0,
        policies_created: 0,
        policies_updated: 0,
        policies_deleted: 0,
        roles_created: 0,
        roles_updated: 0,
        roles_deleted: 0,
    };
    let mut results = Vec::new();
    let mut failure_count: usize = 0;
    // Track successfully created users for dev mode
    let mut created_users: Vec<RbacConfigUser> = Vec::new();

    for change in &changes {
        let result = execute_rbac_change(change, client).await;

        let (action, item_type, name) = match change {
            RbacChange::CreateUser { login, .. } => ("create", "user", login.clone()),
            RbacChange::UpdateUser { login, .. } => ("update", "user", login.clone()),
            RbacChange::DeleteUser { login } => ("delete", "user", login.clone()),
            RbacChange::CreateKey { key_name, .. } => ("create", "key", key_name.clone()),
            RbacChange::DeleteKey { key_name, .. } => ("delete", "key", key_name.clone()),
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
                    RbacChange::CreateUser {
                        login,
                        email,
                        first_name,
                        last_name,
                        company_name,
                    } => {
                        summary.users_created += 1;
                        // Track for dev mode key/profile generation
                        created_users.push(RbacConfigUser {
                            login: login.clone(),
                            email: email.clone(),
                            first_name: first_name.clone(),
                            last_name: last_name.clone(),
                            company_name: company_name.clone(),
                            keys: None,
                        });
                    }
                    RbacChange::UpdateUser { .. } => summary.users_updated += 1,
                    RbacChange::DeleteUser { .. } => summary.users_deleted += 1,
                    RbacChange::CreateKey { .. } => summary.keys_created += 1,
                    RbacChange::DeleteKey { .. } => summary.keys_deleted += 1,
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
                    status: ApplyStatus::Success,
                    error: None,
                });
            }
            Err(e) => {
                if !use_json {
                    println!("  {} {} - FAILED: {}", action, name, e);
                }
                failure_count += 1;
                results.push(ApplyChangeResult {
                    action: action.to_string(),
                    item_type: item_type.to_string(),
                    name,
                    status: ApplyStatus::Failed,
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
        if summary.keys_created > 0 || summary.keys_deleted > 0 {
            println!(
                "  Keys: {} created, {} deleted",
                summary.keys_created, summary.keys_deleted
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

    // Dev mode: generate SSH keys and create CLI profiles for newly created users
    if let Some(profile) = base_profile {
        if !created_users.is_empty() {
            execute_dev_actions(
                &created_users,
                &profile,
                client,
                false,
                use_json,
                &args.dev_key_type,
            )
            .await?;
        } else if !use_json {
            println!();
            println!("Dev mode: No new users were created, skipping key/profile generation.");
        }
    }

    // Return an error if any operations failed
    if failure_count > 0 {
        return Err(anyhow::anyhow!(
            "{} operation(s) failed during apply",
            failure_count
        ));
    }

    Ok(())
}

/// Generate a random password for new users using the OS CSPRNG.
fn generate_password() -> Result<String> {
    use std::fmt::Write;
    let mut buf = [0u8; 24];
    getrandom::fill(&mut buf)
        .map_err(|e| anyhow::anyhow!("Failed to generate random password: {}", e))?;
    let mut result = String::with_capacity(buf.len() * 2);
    for b in &buf {
        // Writing to a String never fails
        let _ = write!(result, "{:02x}", b);
    }
    Ok(result)
}

/// Load SSH public keys for a user from config-specified path or default directory.
///
/// Follows node-triton's `loadUserKeys` behavior:
/// - If `user.keys` is None: try `rbac-user-keys/` relative to config_dir (silent if missing)
/// - If `user.keys` is Some(path): resolve relative to config_dir
///   - Directory: read `{path}/{login}.pub`
///   - File: read it directly
///   - Missing: error (explicit paths must exist)
async fn load_user_keys(user: &RbacConfigUser, config_dir: &Path) -> Result<Vec<ParsedKey>> {
    let key_path = match &user.keys {
        None => {
            // Default: try rbac-user-keys/{login}.pub, silently skip if missing
            let default_dir = config_dir.join("rbac-user-keys");
            if !tokio::fs::try_exists(&default_dir).await.unwrap_or(false) {
                return Ok(vec![]);
            }
            let pub_file = default_dir.join(format!("{}.pub", user.login));
            if !tokio::fs::try_exists(&pub_file).await.unwrap_or(false) {
                return Ok(vec![]);
            }
            pub_file
        }
        Some(path_str) => {
            let path = if Path::new(path_str).is_relative() {
                config_dir.join(path_str)
            } else {
                PathBuf::from(path_str)
            };
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Err(anyhow::anyhow!(
                    "Key path '{}' for user '{}' does not exist",
                    path.display(),
                    user.login
                ));
            }
            // Check if path is a directory by trying to read it
            if tokio::fs::read_dir(&path).await.is_ok() {
                let pub_file = path.join(format!("{}.pub", user.login));
                if !tokio::fs::try_exists(&pub_file).await.unwrap_or(false) {
                    return Err(anyhow::anyhow!(
                        "Key file '{}.pub' not found in directory '{}'",
                        user.login,
                        path.display()
                    ));
                }
                pub_file
            } else {
                path
            }
        }
    };

    let content = tokio::fs::read_to_string(&key_path)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read key file '{}': {}", key_path.display(), e))?;

    let mut keys = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Parse SSH public key: <type> <base64> [comment]
        let parts: Vec<&str> = line.splitn(3, char::is_whitespace).collect();
        let name = if parts.len() >= 3 && !parts[2].is_empty() {
            parts[2].to_string()
        } else {
            "imported-key".to_string()
        };
        keys.push(ParsedKey {
            name,
            key: line.to_string(),
        });
    }

    Ok(keys)
}

async fn execute_rbac_change(change: &RbacChange, client: &TypedClient) -> Result<()> {
    let account = client.effective_account();

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
                password: generate_password()?,
                company_name: company_name.clone(),
                first_name: first_name.clone(),
                last_name: last_name.clone(),
                phone: None,
                address: None,
                postal_code: None,
                city: None,
                state: None,
                country: None,
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
                address: None,
                postal_code: None,
                city: None,
                state: None,
                country: None,
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
                rules: rules.clone().into(),
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
                rules: rules.clone().map(Into::into),
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
            // Merge members (default=false) and default_members (default=true)
            let mut member_refs: Vec<cloudapi_client::types::MemberRef> = members
                .iter()
                .map(|m| cloudapi_client::types::MemberRef {
                    type_: cloudapi_client::types::MemberType::Subuser,
                    login: Some(m.clone()),
                    id: None,
                    default: Some(false),
                })
                .collect();
            member_refs.extend(
                default_members
                    .iter()
                    .map(|m| cloudapi_client::types::MemberRef {
                        type_: cloudapi_client::types::MemberType::Subuser,
                        login: Some(m.clone()),
                        id: None,
                        default: Some(true),
                    }),
            );

            let request = cloudapi_client::types::CreateRoleRequest {
                name: name.clone(),
                policies: if policies.is_empty() {
                    None
                } else {
                    Some(
                        policies
                            .iter()
                            .map(|p| cloudapi_client::types::PolicyRef {
                                name: Some(p.clone()),
                                id: None,
                            })
                            .collect(),
                    )
                },
                members: if member_refs.is_empty() {
                    None
                } else {
                    Some(member_refs)
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
            // Convert members + default_members to MemberRef vec
            let member_refs: Option<Vec<cloudapi_client::types::MemberRef>> =
                match (members, default_members) {
                    (None, None) => None,
                    _ => {
                        let mut refs: Vec<cloudapi_client::types::MemberRef> = members
                            .as_ref()
                            .map(|ms| {
                                ms.iter()
                                    .map(|m| cloudapi_client::types::MemberRef {
                                        type_: cloudapi_client::types::MemberType::Subuser,
                                        login: Some(m.clone()),
                                        id: None,
                                        default: Some(false),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        if let Some(dms) = default_members {
                            refs.extend(dms.iter().map(|m| cloudapi_client::types::MemberRef {
                                type_: cloudapi_client::types::MemberType::Subuser,
                                login: Some(m.clone()),
                                id: None,
                                default: Some(true),
                            }));
                        }
                        Some(refs)
                    }
                };

            let policy_refs: Option<Vec<cloudapi_client::types::PolicyRef>> =
                policies.as_ref().map(|ps| {
                    ps.iter()
                        .map(|p| cloudapi_client::types::PolicyRef {
                            name: Some(p.clone()),
                            id: None,
                        })
                        .collect()
                });

            let request = cloudapi_client::types::UpdateRoleRequest {
                name: None,
                policies: policy_refs,
                members: member_refs,
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
        RbacChange::CreateKey {
            user_login,
            key_name,
            key_material,
        } => {
            let user_id = resolve_user(user_login, client).await?;
            let request = cloudapi_client::types::CreateSshKeyRequest {
                name: key_name.clone(),
                key: key_material.clone(),
            };
            client
                .inner()
                .create_user_key()
                .account(account)
                .uuid(&user_id)
                .body(request)
                .send()
                .await?;
            Ok(())
        }
        RbacChange::DeleteKey {
            user_login,
            key_name,
        } => {
            let user_id = resolve_user(user_login, client).await?;
            client
                .inner()
                .delete_user_key()
                .account(account)
                .uuid(&user_id)
                .name(key_name)
                .send()
                .await?;
            Ok(())
        }
    }
}

/// Generate an SSH key for a user using ssh-keygen
async fn generate_ssh_key(
    user_login: &str,
    profile_name: &str,
    key_type: &DevKeyType,
) -> Result<(PathBuf, String, String)> {
    // Create dev-keys directory
    let keys_dir = paths::config_dir().join("dev-keys");
    tokio::fs::create_dir_all(&keys_dir).await?;

    let key_name = format!("{}-{}", profile_name, user_login);
    let key_path = keys_dir.join(&key_name);

    // Remove existing key files if present
    let _ = tokio::fs::remove_file(&key_path).await;
    let _ = tokio::fs::remove_file(key_path.with_extension("pub")).await;

    let key_path_str = key_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid path for key: {}", key_path.display()))?;
    let comment = format!("{}-dev", user_login);

    // Build ssh-keygen args based on key type
    let mut args = vec!["-t"];
    match key_type {
        DevKeyType::Ed25519 => args.push("ed25519"),
        DevKeyType::Rsa => {
            args.extend(["rsa", "-m", "PEM", "-b", "4096"]);
        }
    }
    args.extend(["-N", "", "-f", key_path_str, "-C", &comment]);

    let output = Command::new("ssh-keygen")
        .args(&args)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run ssh-keygen: {}", e))?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Read the public key
    let pub_key_path = key_path.with_extension("pub");
    let public_key = tokio::fs::read_to_string(&pub_key_path)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read public key: {}", e))?
        .trim()
        .to_string();

    // Extract fingerprint from the key (parse from the public key content)
    // The fingerprint will be returned from the API when we upload it
    Ok((key_path, public_key, key_name))
}

/// Create a CLI profile for an RBAC user
async fn create_user_profile(
    base_profile: &Profile,
    user_login: &str,
    key_fingerprint: &str,
) -> Result<String> {
    let profile_name = format!("{}-user-{}", base_profile.name, user_login);

    let profile = Profile {
        name: profile_name.clone(),
        url: base_profile.url.clone(),
        account: base_profile.account.clone(),
        key_id: key_fingerprint.to_string(),
        insecure: base_profile.insecure,
        user: Some(user_login.to_string()),
        roles: None,
        act_as_account: base_profile.act_as_account.clone(),
    };

    profile.save().await?;
    Ok(profile_name)
}

/// Execute dev mode actions (key generation and profile creation)
async fn execute_dev_actions(
    users: &[RbacConfigUser],
    base_profile: &Profile,
    client: &TypedClient,
    _dry_run: bool,
    use_json: bool,
    key_type: &DevKeyType,
) -> Result<()> {
    let account = client.effective_account();

    if users.is_empty() {
        if !use_json {
            println!("No users to create keys/profiles for.");
        }
        return Ok(());
    }

    if !use_json {
        println!();
        println!(
            "Dev mode: Creating keys and profiles for {} user(s):",
            users.len()
        );
        println!();
    }

    // Execute: Generate keys, upload them, create profiles
    for user in users {
        // Generate SSH key
        if !use_json {
            println!("  Generating SSH key for user '{}'...", user.login);
        }
        let (key_path, public_key, key_name) =
            generate_ssh_key(&user.login, &base_profile.name, key_type).await?;

        if !use_json {
            println!("    Key saved to: {}", key_path.display());
        }

        // Upload the public key to CloudAPI
        // First we need to get the user's UUID
        let user_id = resolve_user(&user.login, client).await?;

        if !use_json {
            println!(
                "  Uploading key '{}' for user '{}'...",
                key_name, user.login
            );
        }

        let request = cloudapi_client::types::CreateSshKeyRequest {
            name: key_name.clone(),
            key: public_key,
        };

        let key_response = client
            .inner()
            .create_user_key()
            .account(account)
            .uuid(&user_id)
            .body(request)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to upload key for user '{}': {}", user.login, e)
            })?;

        let uploaded_key = key_response.into_inner();
        let fingerprint = &uploaded_key.fingerprint;

        if !use_json {
            println!("    Key fingerprint: {}", fingerprint);
        }

        // Create CLI profile
        let profile_name = format!("{}-user-{}", base_profile.name, user.login);
        if !use_json {
            println!("  Creating CLI profile '{}'...", profile_name);
        }

        create_user_profile(base_profile, &user.login, fingerprint).await?;

        if !use_json {
            println!("    Profile created successfully");
            println!();
        }
    }

    if !use_json {
        println!(
            "Dev mode complete. Created {} key(s) and profile(s).",
            users.len()
        );
        println!();
        println!(
            "Keys are stored in: {}",
            paths::config_dir().join("dev-keys").display()
        );
        println!();
        println!("To use a profile, run:");
        for user in users {
            println!(
                "  triton -p {}-user-{} <command>",
                base_profile.name, user.login
            );
        }
    }

    Ok(())
}

pub async fn rbac_reset(args: ResetArgs, client: &TypedClient) -> Result<()> {
    let account = client.effective_account();

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

    // Dry run mode - just show what would be deleted
    if args.dry_run {
        println!("[dry-run] {} item(s) would be deleted.", total);
        return Ok(());
    }

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
