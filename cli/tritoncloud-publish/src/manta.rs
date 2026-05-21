// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrappers around the node-manta CLI (`mput`, `mget`, `mmv`).
//!
//! Why shell-out rather than a Rust Manta client: there is no
//! maintained pure-Rust Manta client today, and the node-manta CLI
//! handles SSH-agent signing, HMAC, range requests, and large-file
//! streaming uploads in a way that would be substantial work to
//! reproduce. The publisher runs on an operator workstation where
//! node-manta is already a prerequisite, so the additional dep is
//! free.
//!
//! All three commands respect the standard Manta env vars (`MANTA_URL`,
//! `MANTA_USER`, `MANTA_KEY_ID`).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::info;

/// Upload `local_path` to the Manta object at `remote_path`.
///
/// `remote_path` is a full Manta path (e.g.
/// `/nick.wilkens@mnxsolutions.com/public/tritoncloud/channels/edge.json.new`).
/// Use [`mmv`] afterward to atomically swap into place.
pub fn mput(local_path: &Path, remote_path: &str) -> Result<()> {
    info!(
        local = %local_path.display(),
        remote = remote_path,
        "mput"
    );
    let status = Command::new("mput")
        .arg("-f")
        .arg(local_path)
        .arg(remote_path)
        .status()
        .with_context(|| format!("failed to spawn mput {remote_path}"))?;
    if !status.success() {
        bail!("mput {remote_path} exited {status}");
    }
    Ok(())
}

/// Read the contents of a Manta object into memory.
///
/// We use HTTPS GET against the public URL rather than `mget` so this
/// works whether or not the operator has Manta credentials configured
/// (channel manifests live under `~~/public/`). For the publisher's
/// own use this is overkill, but it makes `tritoncloud-publish show`
/// usable from any host.
pub fn mget_public(public_url: &str) -> Result<Vec<u8>> {
    info!(url = public_url, "mget (public)");
    let output = Command::new("curl")
        .args(["-fsSL", public_url])
        .output()
        .with_context(|| format!("failed to spawn curl {public_url}"))?;
    if !output.status.success() {
        bail!(
            "curl {public_url} exited {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}
