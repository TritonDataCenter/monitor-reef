// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! CLI commands

pub mod account;
pub mod changefeed;
pub mod cloudapi;
pub mod datacenters;
pub mod env;
pub mod fwrule;
pub mod image;
pub mod info;
pub mod instance;
pub mod key;
pub mod network;
pub mod package;
pub mod profile;
pub mod rbac;
pub mod services;
pub mod vlan;
pub mod volume;

pub use account::AccountCommand;
pub use fwrule::FwruleCommand;
pub use image::ImageCommand;
pub use instance::InstanceCommand;
pub use key::KeyCommand;
pub use network::NetworkCommand;
pub use package::PackageCommand;
pub use profile::ProfileCommand;
pub use rbac::RbacCommand;
pub use vlan::VlanCommand;
pub use volume::VolumeCommand;
