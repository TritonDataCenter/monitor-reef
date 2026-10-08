// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The SmartOS host a build runs on: detecting it, choosing the dataset
//! for the build zvol, serialising builds, and installing the result
//! with `imgadm`.

use std::path::Path;

use anyhow::{Context, Result};

/// Shell out to `imgadm install -m <manifest> -f <gz>`, the same
/// invocation an operator would type to install a built image by hand.
/// GZ-only; the caller must have rejected NGZs already.
pub async fn install_via_imgadm(gz: &Path, manifest: &Path) -> Result<()> {
    println!("Installing into the local SmartOS image store via imgadm...");
    let status = tokio::process::Command::new("imgadm")
        .arg("install")
        .arg("-m")
        .arg(manifest)
        .arg("-f")
        .arg(gz)
        .status()
        .await
        .context("spawn imgadm install")?;
    if !status.success() {
        anyhow::bail!("imgadm install exited {status}");
    }
    Ok(())
}

/// Acquire an exclusive `flock` on `<workdir>/.lock`, fail-fast if
/// another process holds it. The returned `File` must outlive the
/// pipeline; closing it (drop) releases the lock. The kernel also
/// releases the lock on any process exit, so a SIGKILL'd run won't
/// leave a stuck lock on disk. The empty `.lock` file itself is
/// harmless and stays around between runs.
pub fn acquire_workdir_lock(workdir: &Path) -> Result<std::fs::File> {
    let lock_path = workdir.join(".lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    // `std::fs::File::try_lock` (stable since Rust 1.89) wraps
    // `flock(LOCK_EX | LOCK_NB)` with no `unsafe` in our code.
    match lock_file.try_lock() {
        Ok(()) => Ok(lock_file),
        Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
            "another image build is already running in {}; wait for it to \
             finish, or use a different workdir to run concurrently",
            workdir.display()
        ),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("flock failed"),
    }
}

/// Detect whether we're running on SmartOS. `uname -v` on illumos
/// distros starts with `joyent_…`. Any other prefix (Darwin, Linux,
/// FreeBSD, …) means a dev box where dry-run is the only sensible
/// thing to do.
pub fn is_smartos() -> Result<bool> {
    let v = std::process::Command::new("uname")
        .arg("-v")
        .output()
        .context("spawn uname -v")?;
    Ok(String::from_utf8_lossy(&v.stdout).starts_with("joyent_"))
}

/// Run `zonename` and return its trimmed output (`global` for the GZ,
/// the zone name for NGZs).
pub fn current_zone() -> Result<String> {
    let zone = std::process::Command::new("zonename")
        .output()
        .context("spawn zonename")?;
    if !zone.status.success() {
        anyhow::bail!("zonename exited {}", zone.status);
    }
    Ok(String::from_utf8_lossy(&zone.stdout).trim().to_string())
}

/// Default dataset for the temporary build zvol.
///
/// In an NGZ this is the delegated dataset (`zones/<zone>/data` with
/// `zoned=on`); in the GZ we drop directly under `zones`.
pub fn default_dataset() -> Result<String> {
    let zone = current_zone()?;
    if zone == "global" {
        return Ok("zones".to_string());
    }
    let dataset = format!("zones/{zone}/data");
    let zoned = std::process::Command::new("zfs")
        .args(["get", "-H", "-o", "value", "zoned", &dataset])
        .output()
        .context("spawn zfs get zoned")?;
    if !zoned.status.success() || String::from_utf8_lossy(&zoned.stdout).trim() != "on" {
        anyhow::bail!(
            "delegated dataset {dataset} not available or not zoned. \
             Pass --dataset to override."
        );
    }
    Ok(dataset)
}
