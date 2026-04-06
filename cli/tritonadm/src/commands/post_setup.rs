// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use clap::Subcommand;

use crate::not_yet_implemented;

#[derive(Subcommand)]
pub enum PostSetupCommand {
    /// Set up CloudAPI
    Cloudapi,
    /// Add external NICs to HEAD node SDC services
    CommonExternalNics,
    /// Set up underlay NICs for compute nodes
    UnderlayNics,
    /// Set up HA for binder (ZooKeeper)
    HaBinder,
    /// Set up HA for manatee (PostgreSQL)
    HaManatee,
    /// Initialize fabric networking
    Fabrics,
    /// Make the headnode a provisionable compute node (dev only)
    DevHeadnodeProv,
    /// Load sample data for development (dev only)
    DevSampleData,
    /// Set up Docker service
    Docker,
    /// Set up Container Monitor (CMON) service
    Cmon,
    /// Set up Container Name Service (CNS)
    Cns,
    /// Set up Volumes API (VOLAPI) service
    Volapi,
    /// Set up log archiver service
    Logarchiver,
    /// Set up Key Backup and Management API (KBMAPI)
    Kbmapi,
    /// Set up Prometheus monitoring
    Prometheus,
    /// Set up Grafana dashboards
    Grafana,
    /// Set up firewall logger agent
    FirewallLoggerAgent,
    /// Set up Manta object storage
    Manta,
    /// Set up Portal web UI
    Portal,
}

impl PostSetupCommand {
    pub fn run(self) -> ! {
        match self {
            Self::Cloudapi => not_yet_implemented("post-setup cloudapi"),
            Self::CommonExternalNics => not_yet_implemented("post-setup common-external-nics"),
            Self::UnderlayNics => not_yet_implemented("post-setup underlay-nics"),
            Self::HaBinder => not_yet_implemented("post-setup ha-binder"),
            Self::HaManatee => not_yet_implemented("post-setup ha-manatee"),
            Self::Fabrics => not_yet_implemented("post-setup fabrics"),
            Self::DevHeadnodeProv => not_yet_implemented("post-setup dev-headnode-prov"),
            Self::DevSampleData => not_yet_implemented("post-setup dev-sample-data"),
            Self::Docker => not_yet_implemented("post-setup docker"),
            Self::Cmon => not_yet_implemented("post-setup cmon"),
            Self::Cns => not_yet_implemented("post-setup cns"),
            Self::Volapi => not_yet_implemented("post-setup volapi"),
            Self::Logarchiver => not_yet_implemented("post-setup logarchiver"),
            Self::Kbmapi => not_yet_implemented("post-setup kbmapi"),
            Self::Prometheus => not_yet_implemented("post-setup prometheus"),
            Self::Grafana => not_yet_implemented("post-setup grafana"),
            Self::FirewallLoggerAgent => not_yet_implemented("post-setup firewall-logger-agent"),
            Self::Manta => not_yet_implemented("post-setup manta"),
            Self::Portal => not_yet_implemented("post-setup portal"),
        }
    }
}
