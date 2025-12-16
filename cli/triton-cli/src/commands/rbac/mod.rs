// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! RBAC (Role-Based Access Control) management commands

mod apply;
mod common;
mod keys;
mod policy;
mod role;
mod role_tags;
mod user;

use anyhow::Result;
use clap::Subcommand;
use cloudapi_client::TypedClient;

pub use apply::{ApplyArgs, ResetArgs};
pub use keys::{UserKeyAddArgs, UserKeyDeleteArgs, UserKeyGetArgs, UserKeysArgs};
pub use policy::RbacPolicyCommand;
pub use role::RbacRoleCommand;
pub use role_tags::RoleTagsCommand;
pub use user::RbacUserCommand;

#[derive(Subcommand, Clone)]
pub enum RbacCommand {
    /// Show RBAC summary information
    Info,
    /// Apply RBAC configuration from a file
    Apply(ApplyArgs),
    /// Reset (delete) all RBAC users, roles, and policies
    Reset(ResetArgs),
    /// Manage RBAC users
    User {
        #[command(subcommand)]
        command: RbacUserCommand,
    },
    /// List RBAC users (alias for 'user list')
    #[command(hide = true)]
    Users,
    /// Manage RBAC roles
    Role {
        #[command(subcommand)]
        command: RbacRoleCommand,
    },
    /// List RBAC roles (alias for 'role list')
    #[command(hide = true)]
    Roles,
    /// Manage RBAC policies
    Policy {
        #[command(subcommand)]
        command: RbacPolicyCommand,
    },
    /// List RBAC policies (alias for 'policy list')
    #[command(hide = true)]
    Policies,
    /// List SSH keys for a sub-user
    Keys(UserKeysArgs),
    /// Get SSH key for a sub-user
    Key(UserKeyGetArgs),
    /// Add SSH key to a sub-user
    #[command(alias = "add-key")]
    KeyAdd(UserKeyAddArgs),
    /// Delete SSH key from a sub-user
    #[command(alias = "delete-key", alias = "rm-key")]
    KeyDelete(UserKeyDeleteArgs),
    /// Manage role tags on resources
    #[command(alias = "role-tag")]
    RoleTags {
        #[command(subcommand)]
        command: RoleTagsCommand,
    },
}

impl RbacCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::Info => apply::rbac_info(client, use_json).await,
            Self::Apply(args) => apply::rbac_apply(args, client, use_json).await,
            Self::Reset(args) => apply::rbac_reset(args, client).await,
            Self::User { command } => command.run(client, use_json).await,
            Self::Users => user::list_users(client, use_json).await,
            Self::Role { command } => command.run(client, use_json).await,
            Self::Roles => role::list_roles(client, use_json).await,
            Self::Policy { command } => command.run(client, use_json).await,
            Self::Policies => policy::list_policies(client, use_json).await,
            Self::Keys(args) => keys::list_user_keys(args, client, use_json).await,
            Self::Key(args) => keys::get_user_key(args, client, use_json).await,
            Self::KeyAdd(args) => keys::add_user_key(args, client, use_json).await,
            Self::KeyDelete(args) => keys::delete_user_key(args, client).await,
            Self::RoleTags { command } => command.run(client, use_json).await,
        }
    }
}
