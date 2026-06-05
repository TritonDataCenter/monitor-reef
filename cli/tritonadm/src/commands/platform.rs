// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use clap::Subcommand;

use crate::not_yet_implemented;

#[derive(Subcommand)]
pub enum PlatformCommand {
    /// List installed platforms
    List,
    /// Show platform usage across compute nodes
    Usage,
    /// Install a platform image
    Install,
    /// Assign a platform to a compute node
    Assign,
    /// Remove a platform image
    Remove,
    /// List available platform images for download
    Avail,
    /// Set the default platform for new compute nodes
    SetDefault,
}

impl PlatformCommand {
    pub fn run(self) -> ! {
        match self {
            Self::List => not_yet_implemented("platform list"),
            Self::Usage => not_yet_implemented("platform usage"),
            Self::Install => not_yet_implemented("platform install"),
            Self::Assign => not_yet_implemented("platform assign"),
            Self::Remove => not_yet_implemented("platform remove"),
            Self::Avail => not_yet_implemented("platform avail"),
            Self::SetDefault => not_yet_implemented("platform set-default"),
        }
    }
}
