// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use clap::Subcommand;

use crate::not_yet_implemented;

#[derive(Subcommand)]
pub enum ExperimentalCommand {
    /// Display images available for experimental update
    Avail,
    /// Experimental update of Triton services and instances
    Update,
    /// Show experimental service info
    Info,
    /// Update agents on compute nodes
    UpdateAgents,
    /// Update other components
    UpdateOther,
    /// Update global zone tools
    UpdateGzTools,
    /// Add new agent services
    AddNewAgentSvcs,
    /// Update Docker service (experimental)
    UpdateDocker,
    /// Install a Docker TLS certificate
    InstallDockerCert,
    /// Fix core VM resolvers
    FixCoreVmResolvers,
    /// Container Name Service (experimental)
    Cns,
    /// NFS shared volumes (experimental)
    NfsVolumes,
    /// Remove Certificate Authority (CA) component
    RemoveCa,
    /// Datacenter maintenance (experimental)
    DcMaint,
}

impl ExperimentalCommand {
    pub fn run(self) -> ! {
        match self {
            Self::Avail => not_yet_implemented("experimental avail"),
            Self::Update => not_yet_implemented("experimental update"),
            Self::Info => not_yet_implemented("experimental info"),
            Self::UpdateAgents => not_yet_implemented("experimental update-agents"),
            Self::UpdateOther => not_yet_implemented("experimental update-other"),
            Self::UpdateGzTools => not_yet_implemented("experimental update-gz-tools"),
            Self::AddNewAgentSvcs => not_yet_implemented("experimental add-new-agent-svcs"),
            Self::UpdateDocker => not_yet_implemented("experimental update-docker"),
            Self::InstallDockerCert => not_yet_implemented("experimental install-docker-cert"),
            Self::FixCoreVmResolvers => not_yet_implemented("experimental fix-core-vm-resolvers"),
            Self::Cns => not_yet_implemented("experimental cns"),
            Self::NfsVolumes => not_yet_implemented("experimental nfs-volumes"),
            Self::RemoveCa => not_yet_implemented("experimental remove-ca"),
            Self::DcMaint => not_yet_implemented("experimental dc-maint"),
        }
    }
}
