// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IMGAPI types
//!
//! All IMGAPI JSON fields use snake_case wire format (matching Rust conventions),
//! except for the `uncompressedDigest` field on `ImageFile` which is camelCase.

pub mod action;
pub mod channel;
pub mod common;
pub mod file;
pub mod image;
pub mod job;
pub mod ping;
pub mod state;

pub use action::*;
pub use channel::*;
pub use common::*;
pub use file::*;
pub use image::*;
pub use ping::*;
