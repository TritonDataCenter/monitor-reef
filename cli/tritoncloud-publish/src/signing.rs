// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-process minisign signing of channel manifests.
//!
//! We do the signing inside the publisher process (via the `minisign`
//! crate) rather than shelling to the `minisign -S` binary because
//! the binary insists on reading the passphrase from a TTY and does
//! not honor any of the common env-var conventions. In-process we can
//! source the passphrase from `MINISIGN_PASSWORD` (CI-friendly) or
//! fall back to an interactive rpassword prompt.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use minisign::{SecretKey, sign};
use tracing::info;

/// Environment variable consulted for the publisher key passphrase.
/// Empty string is treated as "no passphrase set" (encrypted keys
/// always require a non-empty passphrase to decrypt).
const PASSPHRASE_ENV: &str = "MINISIGN_PASSWORD";

/// Sign `manifest_path` with the secret key at `secret_key_path` and
/// write a detached signature to `sig_path`.
///
/// Reads the key file, decrypts it with the passphrase from
/// `MINISIGN_PASSWORD` (env) or an interactive `rpassword` prompt,
/// then signs the manifest's bytes in-process. Output bytes are
/// exactly what `minisign -S -m manifest -x sig` would produce, so
/// downstream verifiers (`tcadm`, `install.sh`, anyone using
/// `minisign-verify`) cannot tell which signing path was used.
pub fn sign_file(secret_key_path: &Path, manifest_path: &Path, sig_path: &Path) -> Result<()> {
    info!(
        manifest = %manifest_path.display(),
        sig = %sig_path.display(),
        "minisign sign (in-process)"
    );

    let passphrase = passphrase_from_env_or_prompt(secret_key_path)?;
    let sk = SecretKey::from_file(secret_key_path, Some(passphrase))
        .with_context(|| format!("loading secret key {}", secret_key_path.display()))?;

    let manifest_bytes =
        fs::read(manifest_path).with_context(|| format!("reading {}", manifest_path.display()))?;
    let cursor = Cursor::new(&manifest_bytes);
    let sig_box =
        sign(None, &sk, cursor, None, None).map_err(|e| anyhow!("minisign sign failed: {e}"))?;

    fs::write(sig_path, sig_box.into_string())
        .with_context(|| format!("writing {}", sig_path.display()))?;

    Ok(())
}

/// Resolve the passphrase for the publisher's encrypted secret key.
///
/// Priority:
///   1. `$MINISIGN_PASSWORD` (non-empty) — set by CI or operator
///      ahead of time.
///   2. Interactive `rpassword` prompt against `/dev/tty`. Fails if
///      no controlling TTY (e.g. a non-interactive script run with no
///      env set).
fn passphrase_from_env_or_prompt(secret_key_path: &Path) -> Result<String> {
    if let Ok(p) = std::env::var(PASSPHRASE_ENV) {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    let prompt = format!("passphrase for {}: ", secret_key_path.display());
    rpassword::prompt_password(&prompt).with_context(|| {
        format!("could not read passphrase (set {PASSPHRASE_ENV} or run from a TTY)")
    })
}
