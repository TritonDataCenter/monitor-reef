// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! RBAC user access key management commands

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use cloudapi_client::types::AccessKeyStatus;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

use super::common::resolve_user;

/// List access keys for a sub-user
#[derive(Args, Clone)]
pub struct UserAccesskeysArgs {
    /// User login or UUID
    pub user: String,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

/// RBAC accesskey command supporting action flags for node-triton compatibility
///
/// This command supports:
///   triton rbac accesskey USER ACCESSKEYID       # show accesskey (default)
///   triton rbac accesskey -c USER                 # create accesskey
///   triton rbac accesskey -u USER ACCESSKEYID     # update accesskey
///   triton rbac accesskey -d USER ACCESSKEYID...  # delete accesskey(s)
#[derive(Args, Clone)]
pub struct RbacAccesskeyCommand {
    /// Create a new access key
    #[arg(short = 'c', long = "create", conflicts_with_all = ["update", "delete"])]
    pub create: bool,

    /// Update an access key
    #[arg(short = 'u', long = "update", conflicts_with_all = ["create", "delete"])]
    pub update: bool,

    /// Delete access key(s)
    #[arg(short = 'd', long = "delete", conflicts_with_all = ["create", "update"])]
    pub delete: bool,

    /// Status for create/update (Active or Inactive)
    #[arg(short = 's', long, ignore_case = true)]
    pub status: Option<AccessKeyStatus>,

    /// Description for create/update
    #[arg(short = 'D', long, visible_alias = "desc")]
    pub description: Option<String>,

    /// Skip confirmation (for delete)
    #[arg(short = 'f', long = "force", visible_alias = "yes", short_alias = 'y')]
    pub force: bool,

    /// Arguments: USER [ACCESSKEYID...]
    pub args: Vec<String>,
}

impl RbacAccesskeyCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        if self.create {
            if self.args.is_empty() {
                anyhow::bail!(
                    "Usage: triton rbac accesskey -c [--status STATUS] [--description DESC] USER"
                );
            }
            let user = &self.args[0];
            create_user_access_key(user, self.status, self.description, client, use_json).await
        } else if self.update {
            if self.args.len() < 2 {
                anyhow::bail!(
                    "Usage: triton rbac accesskey -u [--status STATUS] [--description DESC] USER ACCESSKEYID"
                );
            }
            let user = &self.args[0];
            let accesskeyid = &self.args[1];
            update_user_access_key(
                user,
                accesskeyid,
                self.status,
                self.description,
                client,
                use_json,
            )
            .await
        } else if self.delete {
            if self.args.len() < 2 {
                anyhow::bail!("Usage: triton rbac accesskey -d USER ACCESSKEYID...");
            }
            let user = &self.args[0];
            let ids: Vec<String> = self.args[1..].to_vec();
            delete_user_access_keys(user, ids, self.force, client).await
        } else if self.args.len() >= 2 {
            // Default: show accesskey
            let user = &self.args[0];
            let accesskeyid = &self.args[1];
            get_user_access_key(user, accesskeyid, client, use_json).await
        } else {
            anyhow::bail!(
                "Usage: triton rbac accesskey USER ACCESSKEYID           (show)\n\
                 Or:    triton rbac accesskey -c USER                     (create)\n\
                 Or:    triton rbac accesskey -u USER ACCESSKEYID         (update)\n\
                 Or:    triton rbac accesskey -d USER ACCESSKEYID...      (delete)\n\n\
                 Run 'triton rbac accesskey --help' for more information"
            );
        }
    }
}

pub async fn list_user_access_keys(
    args: UserAccesskeysArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(&args.user, client).await?;

    let response = client
        .inner()
        .list_user_access_keys()
        .account(account)
        .uuid(&user_id)
        .send()
        .await?;

    let mut keys = response.into_inner();
    keys.sort_by(|a, b| a.created.cmp(&b.created));

    if use_json {
        json::print_json_stream(&keys)?;
    } else {
        let mut tbl = TableBuilder::new(&["ACCESSKEYID", "STATUS", "UPDATED"])
            .with_long_headers(&["DESCRIPTION", "CREATED"]);
        for key in &keys {
            tbl.add_row(vec![
                key.accesskeyid.clone(),
                crate::output::enum_to_display(&key.status),
                key.updated.to_rfc3339(),
                key.description.clone().unwrap_or_default(),
                key.created.to_rfc3339(),
            ]);
        }
        tbl.print(&args.table)?;
    }

    Ok(())
}

async fn get_user_access_key(
    user: &str,
    accesskeyid: &str,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(user, client).await?;

    let response = client
        .inner()
        .get_user_access_key()
        .account(account)
        .uuid(&user_id)
        .accesskeyid(accesskeyid)
        .send()
        .await?;

    let key = response.into_inner();

    if use_json {
        json::print_json(&key)?;
    } else {
        json::print_json_pretty(&key)?;
    }

    Ok(())
}

async fn create_user_access_key(
    user: &str,
    status: Option<AccessKeyStatus>,
    description: Option<String>,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(user, client).await?;

    let request = cloudapi_client::types::CreateAccessKeyRequest {
        status,
        description,
    };

    let response = client
        .inner()
        .create_user_access_key()
        .account(account)
        .uuid(&user_id)
        .body(request)
        .send()
        .await?;

    let key = response.into_inner();

    if use_json {
        json::print_json(&key)?;
    } else {
        println!("AccessKeyId:     {}", key.accesskeyid);
        println!("AccessKeySecret: {}", key.accesskeysecret);
        println!();
        println!("WARNING: Save the secret now. It cannot be retrieved again.");
    }

    Ok(())
}

async fn update_user_access_key(
    user: &str,
    accesskeyid: &str,
    status: Option<AccessKeyStatus>,
    description: Option<String>,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(user, client).await?;

    let request = cloudapi_client::types::UpdateAccessKeyRequest {
        status,
        description,
    };

    let response = client
        .inner()
        .update_user_access_key()
        .account(account)
        .uuid(&user_id)
        .accesskeyid(accesskeyid)
        .body(request)
        .send()
        .await?;

    let key = response.into_inner();

    if use_json {
        json::print_json(&key)?;
    } else {
        println!("Updated access key {}", key.accesskeyid);
        json::print_json_pretty(&key)?;
    }

    Ok(())
}

async fn delete_user_access_keys(
    user: &str,
    ids: Vec<String>,
    force: bool,
    client: &TypedClient,
) -> Result<()> {
    let account = client.effective_account();
    let user_id = resolve_user(user, client).await?;

    for id in &ids {
        if !force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete user {} access key '{}'?", user, id))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        client
            .inner()
            .delete_user_access_key()
            .account(account)
            .uuid(&user_id)
            .accesskeyid(id)
            .send()
            .await?;

        println!("Deleted user {} access key '{}'", user, id);
    }

    Ok(())
}
