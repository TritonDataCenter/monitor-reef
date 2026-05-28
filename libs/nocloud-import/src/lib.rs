// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Cloud-init nocloud image import pipeline.
//!
//! Lifted as-is from the upstream tritonadm `fetch-nocloud` command
//! (TritonDataCenter/monitor-reef commit 727713ff) and re-homed here
//! as a shared library so multiple CLIs (tritonadm's own, our tcadm
//! `image fetch-nocloud` verb, future automation) can drive the same
//! end-to-end fetch + qcow2/vmdk/xz decode + zfs receive + IMGAPI
//! manifest pipeline without each one re-implementing the vendor
//! profiles.
//!
//! See `docs/design/tritonadm-nocloud-import.md` for the original
//! design doc (also lifted from the same commit).
//!
//! ## Surface
//!
//! - [`vendor::Vendor`] — built-in vendor enum (alma, ubuntu, …).
//! - [`vendor::ResolvedImage`] — vendor-agnostic struct emitted after
//!   release resolution; pipeline input.
//! - [`pipeline`] — `run`-style entry points that download, verify,
//!   decode, and produce a `*.zfs.gz` + `*.json` pair.
//! - [`verify::Verifier`] — checksum strategies (Sha256Pinned,
//!   Sha256SumsTls, etc.) abstracted behind an async trait.
//! - [`manifest::ManifestInputs`] — the IMGAPI manifest builder
//!   wired by the pipeline.

pub mod manifest;
pub mod pipeline;
pub mod vendor;
pub mod verify;
pub mod zfs;

/// Render a serde-Serialize enum to its serde-rename string form
/// (typically kebab-case via `#[serde(rename_all = "kebab-case")]`).
/// Inlined from the upstream tritonadm helper so the lift didn't
/// need to drag in any of tritonadm's CLI scaffolding.
pub fn enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{val:?}"))
}
