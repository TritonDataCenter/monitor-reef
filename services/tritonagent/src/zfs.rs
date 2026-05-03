// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrappers around the SmartOS `zfs(1M)` CLI.
//!
//! Phase 0 deliberately uses the CLI rather than libzfs/ioctls:
//! the operations the agent needs (snapshot existence check,
//! `zfs receive`, snapshot, destroy) are all one-shot, and the
//! shell-out cost is dwarfed by network and zone-create time
//! anyway. A future slice with high-throughput needs (e.g.
//! parallel image imports during a fleet refresh) can swap in
//! the FFI bindings.
//!
//! ## Why `sh -c` for the gzipped receive
//!
//! `zfs receive` reads the raw `zfs send` stream from stdin;
//! image content on disk is gzip-compressed. The natural
//! pipeline is `gzip -dc image.gz | zfs receive zones/<id>`.
//! Tokio's `ChildStdout`-as-`Stdio` story is awkward (no
//! direct conversion path), and rolling our own copy loop adds
//! complexity for no benefit. Going through `/bin/sh -c` with
//! `set -o pipefail` gives us proper failure propagation when
//! `gzip` chokes on a corrupt image: without `pipefail` the
//! shell would report only `zfs`'s exit status, which is
//! typically 0 because zfs sees EOF and reports a happy
//! short-stream — silently corrupting our pool with a partial
//! dataset. The strings we pass through are well-formed
//! (uuid-keyed paths under our cache directory), so quoting
//! escapes are not load-bearing here.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use tokio::process::Command;

/// Returns `true` if a snapshot with the exact given name
/// already exists. Used by [`crate::images::ensure`] to skip
/// the download + receive entirely on a host that already
/// has the image's content.
pub async fn snapshot_exists(name: &str) -> Result<bool> {
    let output = Command::new("zfs")
        .args(["list", "-H", "-t", "snapshot", "-o", "name", name])
        .output()
        .await
        .context("spawn zfs list")?;
    Ok(output.status.success())
}

/// Snapshot `dataset` as `dataset@snap_name`. Errors when
/// the source dataset does not exist or the snapshot name is
/// already taken.
pub async fn snapshot(dataset: &str, snap_name: &str) -> Result<()> {
    let full = format!("{dataset}@{snap_name}");
    let output = Command::new("zfs")
        .args(["snapshot", &full])
        .output()
        .await
        .context("spawn zfs snapshot")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs snapshot {full} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    Ok(())
}

/// Recursively destroy a dataset (including its snapshots).
/// Used by [`crate::images::ensure`] to clean up after a
/// failed `zfs receive`. A dataset that doesn't exist is
/// reported as success: same idempotency story as the
/// `vmadm delete` wrapper.
pub async fn destroy(dataset: &str) -> Result<()> {
    let output = Command::new("zfs")
        .args(["destroy", "-r", dataset])
        .output()
        .await
        .context("spawn zfs destroy")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("does not exist") || stderr.contains("dataset does not exist") {
        return Ok(());
    }
    Err(anyhow!(
        "zfs destroy {dataset} failed (exit {}): {stderr}",
        output.status,
    ))
}

/// Decompress `gz_path` and pipe the resulting `zfs send`
/// stream into `zfs receive <dataset>`. The new dataset name
/// is `dataset` exactly — the caller controls naming. On
/// failure `dataset` may exist as a partial dataset; the
/// caller is responsible for [`destroy`]ing it.
pub async fn recv_gzipped(dataset: &str, gz_path: &Path) -> Result<()> {
    let path_str = gz_path
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 path: {gz_path:?}"))?;
    let pipeline = format!("set -o pipefail; gzip -dc {path_str} | zfs receive {dataset}");
    let output = Command::new("/bin/sh")
        .args(["-c", &pipeline])
        .output()
        .await
        .context("spawn /bin/sh for gzip|zfs-recv")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "gzip | zfs receive {dataset} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    Ok(())
}
