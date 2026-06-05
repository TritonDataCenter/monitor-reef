// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use clap::Subcommand;

use crate::not_yet_implemented;

#[derive(Subcommand)]
pub enum ChannelCommand {
    /// List available update channels
    List,
    /// Set the current update channel
    Set,
    /// Unset the current update channel
    Unset,
    /// Get the current update channel
    Get,
}

impl ChannelCommand {
    pub fn run(self) -> ! {
        match self {
            Self::List => not_yet_implemented("channel list"),
            Self::Set => not_yet_implemented("channel set"),
            Self::Unset => not_yet_implemented("channel unset"),
            Self::Get => not_yet_implemented("channel get"),
        }
    }
}
