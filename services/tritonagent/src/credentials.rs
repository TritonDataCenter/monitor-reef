// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-disk persistence for the agent's per-CN API credential.
//!
//! After the registration / approval handshake, the agent receives a
//! wire-form `tcadm_…` API key from tritond. That key is the agent's
//! sole credential for every subsequent `/v1/agent/*` call, so it must
//! survive process restarts.
//!
//! This module owns the on-disk format. Today it is a single file
//! containing the plaintext key, mode 0600, in a parent directory with
//! mode 0700. We keep it deliberately simple — the only consumers are
//! [`load`] and [`save`], so an atomic write + chmod is enough; we do
//! not need a full keyring abstraction.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Default on-disk location for the persisted credential.
///
/// Lives under `/var/lib/tritonagent` because that directory is
/// expected to be present on a SmartOS GZ install of the agent and
/// has the right semantics (per-host, persists across upgrades, not
/// in `/tmp`). Operators who need a different location pass
/// `--credential-path` to the binary.
pub const DEFAULT_CREDENTIAL_PATH: &str = "/var/lib/tritonagent/credentials";

/// Returns the default credential file path as a [`PathBuf`].
///
/// Convenience wrapper so call sites can compose with `clap`'s
/// `default_value_os_t`-style APIs without manual `PathBuf::from`.
pub fn path_default() -> PathBuf {
    PathBuf::from(DEFAULT_CREDENTIAL_PATH)
}

/// Load a previously-persisted credential from disk.
///
/// Returns:
///
/// * `Ok(Some(plaintext))` when the file exists and was read. The
///   returned string is the wire-form `tcadm_…` API key, stripped of
///   any trailing newline so callers can stuff it straight into an
///   HTTP header.
/// * `Ok(None)` when the file is absent. This is the "fresh CN, must
///   register" signal — the registration flow handles it.
/// * `Err(_)` on a genuine I/O error (permissions, corrupt parent dir,
///   etc.). Callers should *not* paper over these — a failure to read
///   an existing credential file is operator-visible and should
///   prevent the agent from racing to re-register and getting a
///   second bound key.
pub fn load(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => {
            let trimmed = s.trim_end_matches(['\r', '\n']).to_string();
            if trimmed.is_empty() {
                // An empty file is a corrupt credential; surface as an
                // error rather than silently re-registering.
                anyhow::bail!(
                    "credential file {} is present but empty; refusing to silently re-register",
                    path.display()
                );
            }
            Ok(Some(trimmed))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read credential file {}", path.display())),
    }
}

/// Atomically persist the agent's API credential.
///
/// Writes `plaintext` to a sibling temp file in the same directory
/// then `rename(2)`s it onto `path`. This is the standard idiom for
/// crash-safe credential storage on POSIX: a partial write or a
/// power loss during the temp file's existence cannot leave `path`
/// in a half-written state.
///
/// Side effects:
///
/// * Creates the parent directory (recursively) with mode 0700 if it
///   does not already exist. Existing parent directories are left
///   alone — operators may have stricter permissions and we should
///   not weaken them.
/// * After the rename, sets the credential file's mode to 0600 so
///   only the owning user can read it.
///
/// Returns `Err` on any of: parent-directory creation, temp-file
/// open/write/close, rename, or chmod failure.
pub fn save(path: &Path, plaintext: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("credential path {} has no parent", path.display()))?;

    if !parent.exists() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create credential parent directory {}", parent.display()))?;
        // 0700 on the parent so a peer user on a multi-tenant CN
        // cannot list the credential file. Best-effort: if chmod
        // fails (e.g. on a non-unix mount), surface the error
        // rather than silently leave a world-readable directory.
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "set 0700 mode on credential parent directory {}",
                parent.display()
            )
        })?;
    }

    // Temp file in the same directory so `rename` is atomic (same
    // filesystem). Naming includes the target's file name plus a
    // `.tmp` suffix so a crash leaves an obvious orphan rather than
    // a name collision risk.
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("credential path {} has no file name", path.display()))?
        .to_owned();
    let mut tmp_name = file_name.clone();
    tmp_name.push(".tmp");
    let tmp_path = parent.join(&tmp_name);

    {
        // Scope the file handle so it is closed (flushed) before the
        // rename below.
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)
            .with_context(|| format!("open temp credential file {}", tmp_path.display()))?;
        f.write_all(plaintext.as_bytes())
            .with_context(|| format!("write credential to {}", tmp_path.display()))?;
        // Newline at EOF so a sysadmin's `cat` is well-behaved.
        f.write_all(b"\n")
            .with_context(|| format!("write trailing newline to {}", tmp_path.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync credential temp file {}", tmp_path.display()))?;
    }

    fs::rename(&tmp_path, path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()))?;
    // Belt-and-suspenders: re-apply 0600 after the rename. Opening
    // with `mode(0o600)` honours `umask`, so a permissive umask on
    // the agent's parent process could otherwise widen the file.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set 0600 mode on credential file {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("absent");
        assert!(load(&p).expect("load").is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("nested").join("credentials");
        save(&p, "tcadm_examplekey").expect("save");

        let got = load(&p).expect("load").expect("present");
        assert_eq!(got, "tcadm_examplekey");

        // File mode is 0600.
        let meta = std::fs::metadata(&p).expect("meta");
        assert_eq!(meta.mode() & 0o777, 0o600);

        // Parent created with 0700.
        let parent_meta = std::fs::metadata(p.parent().expect("parent")).expect("parent meta");
        assert_eq!(parent_meta.mode() & 0o777, 0o700);
    }

    #[test]
    fn load_strips_trailing_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("creds");
        std::fs::write(&p, b"tcadm_thekey\n").expect("write");
        let got = load(&p).expect("load").expect("present");
        assert_eq!(got, "tcadm_thekey");
    }

    #[test]
    fn load_rejects_empty_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("creds");
        std::fs::write(&p, b"").expect("write");
        let err = load(&p).expect_err("must reject empty");
        let msg = format!("{err:#}");
        assert!(msg.contains("empty"), "unexpected error: {msg}");
    }

    #[test]
    fn save_overwrites_existing_credential() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("creds");
        save(&p, "tcadm_first").expect("save first");
        save(&p, "tcadm_second").expect("save second");
        assert_eq!(load(&p).expect("load").expect("present"), "tcadm_second");
    }
}
