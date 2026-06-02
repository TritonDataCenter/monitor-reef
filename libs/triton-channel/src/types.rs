// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Serde types mirroring the JSON written to
//! `~~/public/tritoncloud/channels/<channel>.json`.
//!
//! See `rfd/00006/01-pipeline-and-channels.md` for the full schema
//! specification and the worked example.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// Schema version this code understands. A consumer reading a manifest
/// with `schema > CURRENT_SCHEMA` MUST refuse to act on it (see
/// `super::parse`).
pub const CURRENT_SCHEMA: u32 = 1;

/// Root of a channel manifest.
///
/// One JSON object lives at
/// `https://us-central.manta.mnx.io/<acct>/public/tritoncloud/channels/<channel>.json`,
/// signed by a sibling `<channel>.json.minisig`. The CI publisher
/// rewrites this in place via mput-to-`.new` + `mmv` so readers always
/// see a consistent snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelManifest {
    /// Human-readable channel name, e.g. `"edge"`, `"stable"`.
    pub channel: String,

    /// Schema version of this document. Bump only on breaking changes;
    /// purely-additive changes leave this alone.
    pub schema: u32,

    /// When this snapshot of the channel was published. Useful for
    /// "is my view stale?" diagnostics.
    pub updated_at: DateTime<Utc>,

    /// Free-form identifier for the operator who signed this snapshot.
    /// Not load-bearing for security — that comes from the minisign
    /// signature — but used in `tcadm version` output.
    pub publisher: String,

    /// Zone images indexed by canonical image name
    /// (e.g. `"triton-tritond"`).
    #[serde(default)]
    pub images: BTreeMap<String, ImageEntry>,

    /// Per-CN GZ tarball artifacts (gz-tools-style) indexed by
    /// canonical agent name (e.g. `"tritonagent"`).
    #[serde(default)]
    pub agents: BTreeMap<String, AgentEntry>,

    /// Zone-resident service binaries that update by swapping the binary
    /// into an existing zone + restarting its SMF service (binary-swap,
    /// not a full image reprovision), indexed by canonical service name
    /// (e.g. `"tritond"`, `"admin-backend"`). Additive at schema 1.
    #[serde(default)]
    pub services: BTreeMap<String, ServiceEntry>,

    /// `tcadm` binaries indexed by Rust target triple
    /// (e.g. `"x86_64-unknown-illumos"`).
    #[serde(default)]
    pub tcadm: BTreeMap<String, TcadmEntry>,
}

/// One zone-resident service binary. `tcadm update <name>` swaps it into
/// the target zone and restarts its SMF service — no image reprovision,
/// so `/data` and the rest of the zone root are untouched. Stays
/// minisign-verified (the whole manifest is signed) + sha256-checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEntry {
    /// Build stamp (`YYYYMMDDTHHMMSSZ`).
    pub stamp: String,

    /// Public Manta URL of the binary blob.
    pub url: Url,

    /// Lowercase hex SHA-256 of the binary, verified after download and
    /// compared against the on-disk binary to decide "already current".
    pub sha256: String,

    /// Size in bytes.
    pub size_bytes: u64,

    /// Alias of the zone the binary lives in (e.g. `"triton-tritond"`).
    pub zone: String,

    /// Absolute path of the binary INSIDE that zone
    /// (e.g. `"/opt/triton/tritond/bin/tritond"`).
    pub bin_path: String,

    /// SMF service to restart after the swap
    /// (e.g. `"site/triton-tritond"`).
    pub smf: String,

    /// Oldest PI buildstamp this binary is known to coexist with.
    #[serde(default)]
    pub pi_min: Option<String>,
}

/// One zone image. The pair of URLs points at the imgadm-shaped
/// `manifest.json` + `content.zfs.gz` for that image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageEntry {
    /// Build stamp (`YYYYMMDDTHHMMSSZ`). Matches the basename of the
    /// files at `manifest_url` / `content_url`.
    pub stamp: String,

    /// imgadm image UUID baked into the manifest. Surfaced here so
    /// consumers can detect "this image is already installed" without
    /// downloading the manifest first.
    pub uuid: Uuid,

    /// Public Manta URL of the imgadm manifest (`.json`).
    pub manifest_url: Url,

    /// Public Manta URL of the imgadm content (`.zfs.gz`).
    pub content_url: Url,

    /// Lowercase hex SHA-256 of the content blob. Verified by
    /// `super::verify_sha256` after download.
    pub sha256: String,

    /// Size of the content blob in bytes. Surfaced so `tcadm image
    /// install` can show a progress bar without a HEAD request.
    pub size_bytes: u64,

    /// Oldest PI buildstamp this image is known to coexist with.
    /// `None` means unconstrained. `tcadm` refuses to install if the
    /// running PI buildstamp sorts earlier than this.
    #[serde(default)]
    pub pi_min: Option<String>,

    /// On-disk data format version this image writes.
    pub data_format_version: u32,

    /// Oldest on-disk data format this image can attach to and
    /// upgrade from. `tcadm image update` refuses a reprovision when
    /// the existing zone's `data_format_version` is below this.
    pub data_format_min_read: u32,
}

/// One per-CN GZ tarball artifact (e.g. `tritonagent`, `proteusadm`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEntry {
    /// Build stamp.
    pub stamp: String,

    /// Public Manta URL of the tarball.
    pub url: Url,

    /// Lowercase hex SHA-256 of the tarball.
    pub sha256: String,

    /// Size in bytes.
    pub size_bytes: u64,

    /// Oldest PI buildstamp this agent is known to coexist with.
    #[serde(default)]
    pub pi_min: Option<String>,
}

/// One `tcadm` binary tarball, keyed in the parent map by Rust target
/// triple. We do not model the triple as an enum so an older `tcadm`
/// can still parse a manifest that adds new triples it does not know
/// about (it simply will not find an entry for its own triple).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcadmEntry {
    /// Build stamp.
    pub stamp: String,

    /// Public Manta URL of the tarball.
    pub url: Url,

    /// Lowercase hex SHA-256 of the tarball.
    pub sha256: String,

    /// Size in bytes.
    pub size_bytes: u64,
}
