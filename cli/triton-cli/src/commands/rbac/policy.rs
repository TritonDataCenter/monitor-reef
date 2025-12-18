// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC policy management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use serde::Deserialize;

use crate::output::{json, table};

use super::editor;

/// Policy subcommands (modern pattern)
#[derive(Subcommand, Clone)]
pub enum PolicySubcommand {
    /// List RBAC policies
    #[command(visible_alias = "ls")]
    List,
    /// Get policy details
    Get(PolicyGetArgs),
    /// Create policy
    Create(PolicyCreateArgs),
    /// Update policy
    Update(PolicyUpdateArgs),
    /// Delete policy(s)
    #[command(visible_alias = "rm")]
    Delete(PolicyDeleteArgs),
}

/// RBAC policy command supporting both subcommands and action flags
///
/// This command supports two patterns for compatibility:
///
/// Modern (subcommand) pattern:
///   triton rbac policy list
///   triton rbac policy get POLICY
///   triton rbac policy create NAME --rule ...
///   triton rbac policy delete POLICY
///
/// Legacy (action flag) pattern:
///   triton rbac policy POLICY           # show policy (default)
///   triton rbac policy -a [FILE]        # add policy from file or stdin
///   triton rbac policy -e POLICY        # edit policy in $EDITOR
///   triton rbac policy -d POLICY...     # delete policy(s)
#[derive(Args, Clone)]
#[command(args_conflicts_with_subcommands = true)]
pub struct RbacPolicyCommand {
    #[command(subcommand)]
    pub command: Option<PolicySubcommand>,

    /// Add a new policy (legacy compat: read from FILE, "-" for stdin, or interactive)
    #[arg(short = 'a', long = "add", conflicts_with_all = ["delete", "edit"])]
    pub add: bool,

    /// Edit policy in $EDITOR (legacy compat)
    #[arg(short = 'e', long = "edit", conflicts_with_all = ["add", "delete"])]
    pub edit: bool,

    /// Delete policy(s) (legacy compat)
    #[arg(short = 'd', long = "delete", conflicts_with_all = ["add", "edit"])]
    pub delete: bool,

    /// Skip confirmation (for delete)
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Policy(s) or file argument
    /// For show: POLICY name/uuid
    /// For add: optional FILE path (or "-" for stdin)
    /// For edit: POLICY name/uuid
    /// For delete: one or more POLICY name/uuid
    #[arg(trailing_var_arg = true)]
    pub args: Vec<String>,
}

#[derive(Args, Clone)]
pub struct PolicyGetArgs {
    /// Policy name or UUID
    pub policy: String,
}

#[derive(Args, Clone)]
pub struct PolicyCreateArgs {
    /// Policy name
    pub name: String,
    /// Policy rules (can be specified multiple times)
    #[arg(long, short)]
    pub rule: Vec<String>,
    /// Description
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Args, Clone)]
pub struct PolicyUpdateArgs {
    /// Policy name or UUID
    pub policy: String,
    /// New name
    #[arg(long)]
    pub name: Option<String>,
    /// New rules (replaces existing)
    #[arg(long, short)]
    pub rule: Vec<String>,
    /// New description
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Args, Clone)]
pub struct PolicyDeleteArgs {
    /// Policy name(s) or UUID(s)
    pub policies: Vec<String>,
    /// Skip confirmation
    #[arg(long, short, visible_alias = "yes", short_alias = 'y')]
    pub force: bool,
}

impl RbacPolicyCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        // If a subcommand is provided, use the modern pattern
        if let Some(cmd) = self.command {
            return match cmd {
                PolicySubcommand::List => list_policies(client, use_json).await,
                PolicySubcommand::Get(args) => get_policy(args, client, use_json).await,
                PolicySubcommand::Create(args) => create_policy(args, client, use_json).await,
                PolicySubcommand::Update(args) => update_policy(args, client, use_json).await,
                PolicySubcommand::Delete(args) => delete_policies(args, client).await,
            };
        }

        // Legacy action flag pattern
        if self.add {
            // -a/--add: add policy from file or stdin
            let file = self.args.first().map(|s| s.as_str());
            add_policy_from_file(file, client, use_json).await
        } else if self.edit {
            // -e/--edit: edit policy in $EDITOR
            if self.args.is_empty() {
                anyhow::bail!("POLICY argument required for edit");
            }
            edit_policy_in_editor(&self.args[0], client).await
        } else if self.delete {
            // -d/--delete: delete policy(s)
            if self.args.is_empty() {
                anyhow::bail!("POLICY argument(s) required for delete");
            }
            let args = PolicyDeleteArgs {
                policies: self.args,
                force: self.yes,
            };
            delete_policies(args, client).await
        } else if !self.args.is_empty() {
            // Default: show policy
            let args = PolicyGetArgs {
                policy: self.args[0].clone(),
            };
            get_policy(args, client, use_json).await
        } else {
            // No args and no subcommand - show usage hint
            anyhow::bail!(
                "Usage: triton rbac policy <SUBCOMMAND>\n\
                 Or:    triton rbac policy POLICY           (show policy)\n\
                 Or:    triton rbac policy -a [FILE]        (add policy)\n\
                 Or:    triton rbac policy -e POLICY        (edit policy)\n\
                 Or:    triton rbac policy -d POLICY...     (delete policies)\n\n\
                 Run 'triton rbac policy --help' for more information"
            );
        }
    }
}

pub async fn list_policies(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_policies()
        .account(account)
        .send()
        .await?;

    let policies = response.into_inner();

    if use_json {
        json::print_json_stream(&policies)?;
    } else {
        // node-triton columns: NAME, DESCRIPTION, NRULES (no SHORTID)
        let mut tbl = table::create_table(&["NAME", "DESCRIPTION", "NRULES"]);
        for policy in &policies {
            let nrules = policy.rules.len().to_string();
            tbl.add_row(vec![
                &policy.name,
                policy.description.as_deref().unwrap_or("-"),
                &nrules,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_policy(args: PolicyGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .get_policy()
        .account(account)
        .policy(&args.policy)
        .send()
        .await?;

    let policy = response.into_inner();

    if use_json {
        json::print_json(&policy)?;
    } else {
        println!("ID:          {}", policy.id);
        println!("Name:        {}", policy.name);
        println!(
            "Description: {}",
            policy.description.as_deref().unwrap_or("-")
        );
        println!("Rules:");
        for rule in &policy.rules {
            println!("  - {}", rule);
        }
    }

    Ok(())
}

async fn create_policy(args: PolicyCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    if args.rule.is_empty() {
        return Err(anyhow::anyhow!(
            "At least one rule is required. Use --rule to specify rules."
        ));
    }

    let request = cloudapi_client::types::CreatePolicyRequest {
        name: args.name.clone(),
        rules: args.rule,
        description: args.description,
    };

    let response = client
        .inner()
        .create_policy()
        .account(account)
        .body(request)
        .send()
        .await?;

    let policy = response.into_inner();
    println!("Created policy '{}' ({})", policy.name, policy.id);

    if use_json {
        json::print_json(&policy)?;
    }

    Ok(())
}

async fn update_policy(args: PolicyUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::UpdatePolicyRequest {
        name: args.name,
        rules: if args.rule.is_empty() {
            None
        } else {
            Some(args.rule)
        },
        description: args.description,
    };

    let response = client
        .inner()
        .update_policy()
        .account(account)
        .policy(&args.policy)
        .body(request)
        .send()
        .await?;

    let policy = response.into_inner();
    println!("Updated policy '{}'", policy.name);

    if use_json {
        json::print_json(&policy)?;
    }

    Ok(())
}

pub async fn delete_policies(args: PolicyDeleteArgs, client: &TypedClient) -> Result<()> {
    for policy_ref in &args.policies {
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete policy '{}'?", policy_ref))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        let account = &client.auth_config().account;

        client
            .inner()
            .delete_policy()
            .account(account)
            .policy(policy_ref)
            .send()
            .await?;

        println!("Deleted policy '{}'", policy_ref);
    }

    Ok(())
}

/// Add policy from file (legacy -a flag support)
///
/// Reads policy JSON from:
/// - A file path
/// - stdin (when file is "-")
/// - Interactive prompts (when file is None)
async fn add_policy_from_file(
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

            let description: String = Input::new()
                .with_prompt("Description (optional)")
                .allow_empty(true)
                .interact_text()?;

            println!("Enter rules (one per line, empty line to finish):");
            println!("See https://docs.tritondatacenter.com/public-cloud/rbac/rules for syntax.");
            let mut rules = Vec::new();
            loop {
                let rule: String = Input::new()
                    .with_prompt("Rule")
                    .allow_empty(true)
                    .interact_text()?;
                if rule.is_empty() {
                    break;
                }
                rules.push(rule);
            }

            serde_json::json!({
                "name": name,
                "description": if description.is_empty() { None } else { Some(description) },
                "rules": rules,
            })
        }
    };

    // Extract required fields
    let name = json_data
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required field: name"))?
        .to_string();

    // Extract rules array
    let rules: Vec<String> = json_data
        .get("rules")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if rules.is_empty() {
        return Err(anyhow::anyhow!(
            "At least one rule is required for a policy"
        ));
    }

    // Extract optional description
    let description = json_data
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Create the policy
    let account = &client.auth_config().account;
    let request = cloudapi_client::types::CreatePolicyRequest {
        name: name.clone(),
        rules,
        description,
    };

    let response = client
        .inner()
        .create_policy()
        .account(account)
        .body(request)
        .send()
        .await?;

    let policy = response.into_inner();
    println!("Created policy '{}' ({})", policy.name, policy.id);

    if use_json {
        json::print_json(&policy)?;
    }

    Ok(())
}

/// Struct for deserializing edited policy YAML (comments are ignored by serde_yaml)
#[derive(Deserialize)]
struct PolicyEdit {
    /// Policy name
    name: String,
    /// Policy rules
    #[serde(default)]
    rules: Vec<String>,
    /// Description
    #[serde(default)]
    description: Option<String>,
}

/// Convert a Policy to commented YAML for editing
fn policy_to_commented_yaml(policy: &cloudapi_client::types::Policy, account: &str) -> String {
    let rules = editor::format_yaml_list(&policy.rules, "  ");
    let description = policy.description.as_deref().unwrap_or("");

    format!(
        r#"# Policy: {name}
# ID: {id}
# Account: {account}
# Edit below, save and quit to apply changes

# Policy name (required)
name: {name}

# Description (optional)
description: {description}

# Policy rules (at least one required)
# See https://docs.tritondatacenter.com/public-cloud/rbac/rules for syntax
rules:
{rules}
"#,
        name = policy.name,
        id = policy.id,
        account = account,
        description = description,
        rules = rules,
    )
}

/// Edit policy in $EDITOR (legacy -e flag support)
async fn edit_policy_in_editor(policy_ref: &str, client: &TypedClient) -> Result<()> {
    let account = client.auth_config().account.clone();

    // Fetch current policy
    let response = client
        .inner()
        .get_policy()
        .account(&account)
        .policy(policy_ref)
        .send()
        .await?;
    let policy = response.into_inner();

    let filename = format!("{}-policy-{}.yaml", account, policy.name);
    let original_yaml = policy_to_commented_yaml(&policy, &account);

    let mut current_yaml = original_yaml.clone();
    loop {
        let result = editor::edit_in_editor(&current_yaml, &filename)?;

        if !result.changed {
            println!("No changes made");
            return Ok(());
        }

        match serde_yaml::from_str::<PolicyEdit>(&result.content) {
            Ok(edited) => {
                if edited.rules.is_empty() {
                    eprintln!("Error: At least one rule is required");
                    if !editor::prompt_retry()? {
                        anyhow::bail!("Aborted");
                    }
                    current_yaml = result.content;
                    continue;
                }

                // Build update request
                let request = cloudapi_client::types::UpdatePolicyRequest {
                    name: Some(edited.name.clone()),
                    rules: Some(edited.rules),
                    description: edited.description,
                };

                // Update the policy
                client
                    .inner()
                    .update_policy()
                    .account(&account)
                    .policy(&policy.name)
                    .body(request)
                    .send()
                    .await?;

                println!("Updated policy \"{}\"", edited.name);
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
