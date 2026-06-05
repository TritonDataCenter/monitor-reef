// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Mahi type definitions
//!
//! Mahi mirrors UFDS attributes directly onto Redis-backed blobs. Many fields
//! retain their UFDS casing (lowercase or snake_case), while STS/IAM routes
//! surface AWS-style PascalCase. Be deliberate about `rename_all` attributes.

pub mod accesskey;
pub mod common;
pub mod iam;
pub mod lookup;
pub mod sigv4;
pub mod sts;

pub use accesskey::*;
pub use common::*;
pub use iam::*;
pub use lookup::*;
pub use sigv4::*;
pub use sts::*;
