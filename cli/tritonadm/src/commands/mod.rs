// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Command modules for tritonadm subcommands.

mod channel;
mod dc_maint;
mod dev;
mod experimental;
mod image;
mod imgapi_util;
mod mahi;
mod platform;
mod post_setup;
mod sapi;
pub mod self_update;

pub use channel::ChannelCommand;
pub use dc_maint::DcMaintCommand;
pub use dev::DevCommand;
pub use experimental::ExperimentalCommand;
pub use image::ImageCommand;
pub use mahi::MahiCommand;
pub use platform::PlatformCommand;
pub use post_setup::{PostSetupCommand, PostSetupUrls};
pub use sapi::SapiCommand;
