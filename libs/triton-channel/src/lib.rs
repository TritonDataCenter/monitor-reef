// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Typed schema, signature verification, and content-integrity checks
//! for the Triton Cloud release-channel JSON published to Manta under
//! `~~/public/tritoncloud/channels/<channel>.json`.
//!
//! This crate is the consumer-side contract that `tcadm`, the
//! `install.sh` bootstrap script's Rust equivalent paths, and any
//! future Triton release tooling all sit on top of. It is deliberately
//! IO-free: callers fetch bytes (via `curl`, `reqwest`, `file://`,
//! whatever) and hand them in. That keeps this crate testable without
//! network and keeps it portable to any HTTP client choice the caller
//! prefers.
//!
//! ## Typical use
//!
//! ```ignore
//! use triton_channel::{parse_channel, verify_minisign, verify_sha256};
//!
//! // 1. Fetch the manifest and its signature from Manta.
//! let manifest_bytes: Vec<u8> = curl(channel_url)?;
//! let sig_bytes:      Vec<u8> = curl(channel_url + ".minisig")?;
//!
//! // 2. Verify the manifest was signed by the publisher.
//! const PUBLISHER_PUBKEY: &str =
//!     include_str!("../../../cli/tcadm/publisher.pub");
//! verify_minisign(&manifest_bytes, &sig_bytes, PUBLISHER_PUBKEY)?;
//!
//! // 3. Parse the (now-trusted) manifest.
//! let channel = parse_channel(&manifest_bytes)?;
//!
//! // 4. Look up an artifact, fetch it, verify its sha256.
//! let image = channel.images.get("triton-tritond")
//!     .expect("triton-tritond present");
//! let content_bytes: Vec<u8> = curl(image.content_url.as_str())?;
//! verify_sha256(&content_bytes, &image.sha256)?;
//! ```
//!
//! See [`rfd/00006`](../../../rfd/00006/) for the full design and the
//! Manta layout.

mod errors;
mod types;
mod verify;

pub use errors::{IntegrityError, ParseError, VerifyError};
pub use types::{AgentEntry, CURRENT_SCHEMA, ChannelManifest, ImageEntry, TcadmEntry};
pub use verify::{verify_minisign, verify_sha256};

/// Parse `manifest_bytes` as a [`ChannelManifest`], rejecting any
/// manifest whose `schema` field is newer than [`CURRENT_SCHEMA`].
///
/// Callers should ALWAYS go through this function rather than calling
/// `serde_json::from_slice` directly, because the schema check is the
/// boundary at which we surface "you need to update tcadm" rather than
/// silently dropping fields a newer publisher added with new
/// semantics.
pub fn parse_channel(manifest_bytes: &[u8]) -> Result<ChannelManifest, ParseError> {
    let channel: ChannelManifest = serde_json::from_slice(manifest_bytes)?;
    if channel.schema > CURRENT_SCHEMA {
        return Err(ParseError::UnsupportedSchema {
            found: channel.schema,
            supported: CURRENT_SCHEMA,
        });
    }
    Ok(channel)
}
