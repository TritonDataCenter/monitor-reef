// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-disk persistence for the agent's per-CN IMDSv2 token key.
//!
//! tritond mints the key at CN approval (see
//! `services/tritond/src/cn_credential.rs`) and delivers it in the
//! same registration-response envelope as the API key + console-ticket
//! key (`IMDS_DESIGN.md` §3). The bytes live in a sibling file next to
//! the `--credential-path` API-key file -- same lifecycle, same 0600
//! permissions -- but in its own file so a tritonagent that predates
//! this feature keeps working unchanged.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tritond_auth::IMDS_TOKEN_KEY_BYTES;

/// Sibling-file name. Lives next to the `--credential-path` API-key
/// file. Same naming convention as `console-credentials.json`.
const IMDS_CREDS_FILE_NAME: &str = "imds-credentials.json";

/// On-disk JSON shape. Single secret field; 0600 in a 0700 parent.
#[derive(Debug, Serialize, Deserialize)]
struct ImdsCredsFile {
    /// Per-CN HS256 IMDSv2 session-token key, lowercase hex (64
    /// chars). Absent on a CN that registered before this feature
    /// shipped -- see [`load_imds_token_key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    imds_token_key_hex: Option<String>,
}

fn imds_creds_path(credential_path: &Path) -> PathBuf {
    let dir = credential_path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(IMDS_CREDS_FILE_NAME)
}

fn write_file(path: &Path, file: &ImdsCredsFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("0700 parent dir {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(file).context("serialize imds-credentials.json")?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {} for write", path.display()))?;
    f.write_all(&json)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn read_file(path: &Path) -> Result<Option<ImdsCredsFile>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let file: ImdsCredsFile =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(file))
}

/// Persist the per-CN IMDS token key alongside the API-key file.
pub fn save_imds_token_key(
    credential_path: &Path,
    key_bytes: &[u8; IMDS_TOKEN_KEY_BYTES],
) -> Result<()> {
    let path = imds_creds_path(credential_path);
    let file = ImdsCredsFile {
        imds_token_key_hex: Some(hex::encode(key_bytes)),
    };
    write_file(&path, &file)
}

/// Load the per-CN IMDS token key, if it has been persisted.
///
/// Returns `Ok(None)` when the file is absent *or* the key field is
/// absent -- the "agent registered before IMDS was wired" case the
/// caller logs a warning about.
pub fn load_imds_token_key(credential_path: &Path) -> Result<Option<[u8; IMDS_TOKEN_KEY_BYTES]>> {
    let path = imds_creds_path(credential_path);
    let Some(file) = read_file(&path)? else {
        return Ok(None);
    };
    let Some(hex_str) = file.imds_token_key_hex else {
        return Ok(None);
    };
    let bytes = hex::decode(hex_str.trim())
        .with_context(|| format!("decode imds-token-key hex in {}", path.display()))?;
    if bytes.len() != IMDS_TOKEN_KEY_BYTES {
        bail!(
            "imds-token-key in {} is {} bytes, expected {}",
            path.display(),
            bytes.len(),
            IMDS_TOKEN_KEY_BYTES,
        );
    }
    let mut out = [0u8; IMDS_TOKEN_KEY_BYTES];
    out.copy_from_slice(&bytes);
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let cred = tmp.path().join("creds");
        let key = [7u8; IMDS_TOKEN_KEY_BYTES];
        save_imds_token_key(&cred, &key).unwrap();
        let got = load_imds_token_key(&cred).unwrap().expect("present");
        assert_eq!(got, key);
    }

    #[test]
    fn load_absent_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let cred = tmp.path().join("creds");
        assert!(load_imds_token_key(&cred).unwrap().is_none());
    }

    #[test]
    fn file_has_owner_only_permissions() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let cred = tmp.path().join("creds");
        save_imds_token_key(&cred, &[0u8; IMDS_TOKEN_KEY_BYTES]).unwrap();
        let p = imds_creds_path(&cred);
        let mode = fs::metadata(&p).unwrap().mode();
        // 0o600 file mode; tolerate the platform setting setuid/setgid bits.
        assert_eq!(mode & 0o777, 0o600);
    }
}
