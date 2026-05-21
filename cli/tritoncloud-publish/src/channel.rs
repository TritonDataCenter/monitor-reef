// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Load, mutate, and publish a channel manifest.
//!
//! The flow on every artifact publish is the same:
//!
//! 1. Fetch the current `<channel>.json` from Manta (or start fresh
//!    if it does not exist yet).
//! 2. Mutate the relevant entry (image / agent / tcadm).
//! 3. Bump `updated_at`.
//! 4. Serialize, sign, mput the `.new` pair, mmv into place.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::info;
use triton_channel::{CURRENT_SCHEMA, ChannelManifest, parse_channel};

use crate::manta::{mget_public, mput};
use crate::signing::sign_file;

/// Settings derived from a publisher run.
pub struct ChannelLocator {
    /// The channel name (`edge`, `stable`, ...).
    pub channel: String,

    /// Manta directory base, e.g.
    /// `/nick.wilkens@mnxsolutions.com/public/tritoncloud`.
    pub manta_base: String,

    /// Public HTTPS prefix corresponding to `manta_base`, e.g.
    /// `https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud`.
    pub https_base: String,

    /// Identifier of the operator publishing this snapshot. Goes into
    /// the manifest's `publisher` field.
    pub publisher: String,
}

impl ChannelLocator {
    pub fn channel_manta_path(&self) -> String {
        format!("{}/channels/{}.json", self.manta_base, self.channel)
    }

    pub fn channel_https_url(&self) -> String {
        format!("{}/channels/{}.json", self.https_base, self.channel)
    }
}

/// Load the current channel manifest from Manta. If it does not exist
/// yet (404), return a fresh empty one. Any other error fails.
pub fn fetch_or_init(locator: &ChannelLocator) -> Result<ChannelManifest> {
    let url = locator.channel_https_url();
    match mget_public(&url) {
        Ok(bytes) => {
            info!(url = %url, "fetched existing channel manifest");
            parse_channel(&bytes).with_context(|| format!("parsing {url}"))
        }
        Err(e) => {
            // We do not distinguish 404 from network failures here;
            // the operator should explicitly use `init-channel` to
            // bootstrap an empty channel. If the fetch failed for any
            // reason, complain.
            Err(e).with_context(|| {
                format!(
                    "could not fetch {url}; if this channel does not exist yet, \
                     run `tritoncloud-publish init-channel --channel {} --publisher ...` first",
                    locator.channel
                )
            })
        }
    }
}

/// Create a brand-new empty manifest. Used by the `init-channel`
/// subcommand.
pub fn new_empty(locator: &ChannelLocator) -> ChannelManifest {
    ChannelManifest {
        channel: locator.channel.clone(),
        schema: CURRENT_SCHEMA,
        updated_at: Utc::now(),
        publisher: locator.publisher.clone(),
        images: Default::default(),
        agents: Default::default(),
        tcadm: Default::default(),
    }
}

/// Serialize `manifest`, sign it with the publisher's secret key, and
/// publish both files (json + minisig) to Manta atomically via mput +
/// mmv.
///
/// `workdir` is a directory under which to stage the JSON + signature
/// files before upload; typically `tempfile::tempdir()`.
pub fn publish(
    locator: &ChannelLocator,
    manifest: &mut ChannelManifest,
    secret_key_path: &Path,
    workdir: &Path,
) -> Result<()> {
    manifest.updated_at = Utc::now();
    manifest.channel.clone_from(&locator.channel);
    manifest.publisher.clone_from(&locator.publisher);

    let json = serde_json::to_vec_pretty(manifest).context("serializing channel manifest")?;
    let manifest_local: PathBuf = workdir.join(format!("{}.json", locator.channel));
    let sig_local: PathBuf = workdir.join(format!("{}.json.minisig", locator.channel));
    fs::write(&manifest_local, &json)
        .with_context(|| format!("writing {}", manifest_local.display()))?;

    sign_file(secret_key_path, &manifest_local, &sig_local)?;

    // Manta object PUT is server-side atomic at the object level, so
    // we mput directly to the live path. The mput dance with a `.new`
    // sibling + `mmv` is a Unix-filesystem idiom that does not apply
    // to object storage (and node-manta does not ship `mmv`).
    let live = locator.channel_manta_path();
    let live_sig = format!("{live}.minisig");

    mput(&manifest_local, &live)?;
    mput(&sig_local, &live_sig)?;

    Ok(())
}
