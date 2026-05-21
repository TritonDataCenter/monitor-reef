// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrapper around the `minisign` CLI for producing detached
//! signatures of channel manifests.
//!
//! We do not sign in-process because the publisher's secret key is
//! passphrase-encrypted and lives outside the workspace. The minisign
//! CLI handles the passphrase prompt (or `MINISIGN_PASSWORD` env var)
//! correctly across platforms.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::info;

/// Run `minisign -S` to produce a detached signature of
/// `manifest_path`, writing it to `sig_path`. `secret_key_path`
/// points at the publisher's `.key` file.
///
/// If `MINISIGN_PASSWORD` is set in the environment, minisign reads
/// the passphrase from there. Otherwise it prompts on stdin.
pub fn sign_file(secret_key_path: &Path, manifest_path: &Path, sig_path: &Path) -> Result<()> {
    info!(
        manifest = %manifest_path.display(),
        sig = %sig_path.display(),
        "minisign sign"
    );
    let status = Command::new("minisign")
        .arg("-S")
        .arg("-s")
        .arg(secret_key_path)
        .arg("-x")
        .arg(sig_path)
        .arg("-m")
        .arg(manifest_path)
        .status()
        .with_context(|| {
            format!(
                "failed to spawn minisign -S -s {} -m {}",
                secret_key_path.display(),
                manifest_path.display()
            )
        })?;
    if !status.success() {
        bail!("minisign -S exited {status}");
    }
    Ok(())
}
