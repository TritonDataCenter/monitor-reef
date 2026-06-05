// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Build script for `tritonadm`.
//!
//! Bakes two compile-time constants into the binary:
//!
//! - `TRITONADM_TARGET` — the Rust target triple (`x86_64-unknown-illumos`,
//!   etc.). Used by `tritonadm self-update` to look up the matching
//!   entry in the channel manifest. Reading it from
//!   `std::env::consts::ARCH/OS` at runtime would risk disagreeing
//!   with what cargo actually built for if someone ever
//!   cross-compiles.
//!
//! - `TRITONADM_BUILD_STAMP` — current UTC time in `YYYYMMDDTHHMMSSZ`
//!   form. The publisher records the same string as the artifact's
//!   `stamp` field in the channel manifest, so `tritonadm self-update`
//!   can detect "already on the latest stamp" without re-downloading.
//!   Override via the `TRITONADM_BUILD_STAMP` environment variable when a
//!   CI pipeline wants to pin to a specific value.

fn main() {
    println!(
        "cargo:rustc-env=TRITONADM_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_string())
    );

    let stamp = std::env::var("TRITONADM_BUILD_STAMP")
        .unwrap_or_else(|_| chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string());
    println!("cargo:rustc-env=TRITONADM_BUILD_STAMP={stamp}");

    // We don't want to rerun the build script every time anything
    // changes; just when these inputs do.
    println!("cargo:rerun-if-env-changed=TRITONADM_BUILD_STAMP");
    println!("cargo:rerun-if-env-changed=TARGET");
}
