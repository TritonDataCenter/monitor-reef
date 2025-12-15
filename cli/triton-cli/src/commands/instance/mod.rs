// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance management commands

use anyhow::Result;
use clap::Subcommand;
use cloudapi_client::TypedClient;

pub mod audit;
pub mod create;
pub mod delete;
pub mod disk;
pub mod firewall;
pub mod get;
pub mod lifecycle;
pub mod metadata;
pub mod nic;
pub mod protection;
pub mod rename;
pub mod resize;
pub mod snapshot;
pub mod ssh;
pub mod tag;
pub mod wait;

pub use list::ListArgs;

pub mod list;

#[derive(Subcommand, Clone)]
pub enum InstanceCommand {
    /// List instances
    #[command(alias = "ls")]
    List(list::ListArgs),

    /// Get instance details
    Get(get::GetArgs),

    /// Create a new instance
    Create(create::CreateArgs),

    /// Delete instance(s)
    #[command(alias = "rm")]
    Delete(delete::DeleteArgs),

    /// Start instance(s)
    Start(lifecycle::StartArgs),

    /// Stop instance(s)
    Stop(lifecycle::StopArgs),

    /// Reboot instance(s)
    Reboot(lifecycle::RebootArgs),

    /// Resize an instance to a different package
    Resize(resize::ResizeArgs),

    /// Rename an instance
    Rename(rename::RenameArgs),

    /// SSH to an instance
    Ssh(ssh::SshArgs),

    /// Wait for instance state change
    Wait(wait::WaitArgs),

    /// View instance audit log
    Audit(audit::AuditArgs),

    /// Enable firewall for instance(s)
    EnableFirewall(firewall::EnableFirewallArgs),

    /// Disable firewall for instance(s)
    DisableFirewall(firewall::DisableFirewallArgs),

    /// List firewall rules for an instance
    Fwrules(firewall::FwrulesArgs),

    /// Enable deletion protection for instance(s)
    EnableDeletionProtection(protection::EnableProtectionArgs),

    /// Disable deletion protection for instance(s)
    DisableDeletionProtection(protection::DisableProtectionArgs),

    /// Manage instance NICs
    Nic {
        #[command(subcommand)]
        command: nic::NicCommand,
    },

    /// Manage instance snapshots
    Snapshot {
        #[command(subcommand)]
        command: snapshot::SnapshotCommand,
    },

    /// Manage instance disks
    Disk {
        #[command(subcommand)]
        command: disk::DiskCommand,
    },

    /// Manage instance tags
    Tag {
        #[command(subcommand)]
        command: tag::TagCommand,
    },

    /// Manage instance metadata
    Metadata {
        #[command(subcommand)]
        command: metadata::MetadataCommand,
    },

    /// Get instance IP address
    Ip(get::IpArgs),
}

impl InstanceCommand {
    pub async fn run(self, client: &TypedClient, json: bool) -> Result<()> {
        match self {
            Self::List(args) => list::run(args, client, json).await,
            Self::Get(args) => get::run(args, client, json).await,
            Self::Create(args) => create::run(args, client, json).await,
            Self::Delete(args) => delete::run(args, client).await,
            Self::Start(args) => lifecycle::start(args, client).await,
            Self::Stop(args) => lifecycle::stop(args, client).await,
            Self::Reboot(args) => lifecycle::reboot(args, client).await,
            Self::Resize(args) => resize::run(args, client).await,
            Self::Rename(args) => rename::run(args, client).await,
            Self::Ssh(args) => ssh::run(args, client).await,
            Self::Wait(args) => wait::run(args, client, json).await,
            Self::Audit(args) => audit::run(args, client, json).await,
            Self::EnableFirewall(args) => firewall::enable(args, client).await,
            Self::DisableFirewall(args) => firewall::disable(args, client).await,
            Self::Fwrules(args) => firewall::list_rules(args, client, json).await,
            Self::EnableDeletionProtection(args) => protection::enable(args, client).await,
            Self::DisableDeletionProtection(args) => protection::disable(args, client).await,
            Self::Nic { command } => command.run(client, json).await,
            Self::Snapshot { command } => command.run(client, json).await,
            Self::Disk { command } => command.run(client, json).await,
            Self::Tag { command } => command.run(client, json).await,
            Self::Metadata { command } => command.run(client, json).await,
            Self::Ip(args) => get::ip(args, client).await,
        }
    }
}
