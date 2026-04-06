// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use clap::Subcommand;

use crate::not_yet_implemented;

#[derive(Subcommand)]
pub enum DcMaintCommand {
    /// Start datacenter maintenance
    Start,
    /// Stop datacenter maintenance
    Stop,
    /// Show datacenter maintenance status
    Status,
}

impl DcMaintCommand {
    pub fn run(self) -> ! {
        match self {
            Self::Start => not_yet_implemented("dc-maint start"),
            Self::Stop => not_yet_implemented("dc-maint stop"),
            Self::Status => not_yet_implemented("dc-maint status"),
        }
    }
}
