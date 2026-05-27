// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrappers around the SmartOS `zfs(1M)` CLI.
//!
//! `set -o pipefail` is load-bearing for the gzipped receive: without
//! it, a corrupt input lets gzip fail while zfs sees EOF and reports
//! 0, silently corrupting the pool with a partial dataset. Inputs are
//! uuid-keyed paths under our cache, so shell quoting is not at risk.

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

// Live-migration helpers: spawn `zfs send`/`zfs recv` with piped
// stdio so the migration transport can ferry bytes directly to the
// WebSocket. Each helper returns the `Child`; the caller MUST `await
// child.wait()` after the stream drains or it will zombie.

use tokio::process::Child;

/// Captured quota state for a dataset, used by the migration
/// saga's quota-dance: zero the values for the snapshot send,
/// then restore on the target after import (or on the source on
/// abort). Mirrors the
/// `tritond_store::SourceFilesystemDetails` shape so the saga can
/// round-trip without a translation layer; we hold a separate
/// type here to avoid an agent → store dependency arrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SavedQuotas {
    /// `zfs get -Hp -o value quota <ds>` parsed as bytes. `None`
    /// when zfs returns `0` (i.e. the property was unset, which
    /// zfs reports as 0 — distinct from "explicit 0" which is
    /// invalid anyway).
    pub quota_bytes: Option<u64>,
    /// Same for `refreservation`.
    pub refreservation_bytes: Option<u64>,
}

/// Take a migration-tagged snapshot: `dataset@migration-{label}`.
///
/// Returns the fully-qualified snapshot name on success. The
/// label is appended verbatim so the caller can encode the
/// migration id + iteration ("base", "increment-1", "final",
/// "postpause") in a single human-readable spot.
pub async fn snapshot_for_migration(dataset: &str, label: &str) -> Result<String> {
    let snap_name = format!("migration-{label}");
    snapshot(dataset, &snap_name).await?;
    Ok(format!("{dataset}@{snap_name}"))
}

/// `-w` (raw, encryption-preserving) is unconditional so encrypted
/// migrations work once the placement filter that rejects them is
/// lifted; harmless on unencrypted datasets.
pub fn spawn_send_full(snapshot: &str) -> Result<Child> {
    let mut cmd = Command::new("zfs");
    cmd.args(["send", "-w", snapshot])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    cmd.spawn()
        .with_context(|| format!("spawn zfs send {snapshot}"))
}

/// Spawn `zfs send -i <from> <to>` (incremental) with stdout
/// piped. Same lifetime contract as [`spawn_send_full`].
pub fn spawn_send_incremental(from_snap: &str, to_snap: &str) -> Result<Child> {
    let mut cmd = Command::new("zfs");
    cmd.args(["send", "-w", "-i", from_snap, to_snap])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    cmd.spawn()
        .with_context(|| format!("spawn zfs send -i {from_snap} {to_snap}"))
}

/// Spawn `zfs recv <dataset>` with stdin piped.
///
/// Returns the `Child` with `stdin: Some(_)` — the caller takes
/// the stdin (`child.stdin.take()`), pipes a
/// [`tritond_vmm_migrate::ZfsReceiver`] into it, and
/// `child.wait()`s after the receiver returns.
///
/// Passes `-F` (force rollback) so the receive succeeds when the
/// target dataset already exists from a prior incremental round.
/// Passes `-u` (no automount) so the dataset doesn't try to
/// mount during the migration window — the migration saga
/// activates it after the cutover.
pub fn spawn_recv(dataset: &str) -> Result<Child> {
    let mut cmd = Command::new("zfs");
    cmd.args(["recv", "-F", "-u", dataset])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd.spawn()
        .with_context(|| format!("spawn zfs recv {dataset}"))
}

/// Capture the current `quota` + `refreservation` of a dataset so
/// the saga can clear them for the snapshot send and restore them
/// later. The saga persists this onto the `MigrationRecord` so a
/// crash-mid-migration recovery can put the source back the way it
/// was.
pub async fn save_quotas(dataset: &str) -> Result<SavedQuotas> {
    let quota = get_size_property(dataset, "quota").await?;
    let refres = get_size_property(dataset, "refreservation").await?;
    Ok(SavedQuotas {
        quota_bytes: quota,
        refreservation_bytes: refres,
    })
}

/// Restore `dataset`'s `quota` + `refreservation` to the values
/// captured by [`save_quotas`]. `None` clears the property
/// (`zfs set quota=none`).
pub async fn restore_quotas(dataset: &str, saved: SavedQuotas) -> Result<()> {
    set_size_property(dataset, "quota", saved.quota_bytes).await?;
    set_size_property(dataset, "refreservation", saved.refreservation_bytes).await?;
    Ok(())
}

/// Clear (set to `none`) both quota properties on `dataset`. The
/// migration saga calls this after [`save_quotas`] to allow the
/// snapshot dance (snapshots can't be taken when a dataset is
/// over its quota — a corner case the legacy quota dance was
/// invented to work around).
pub async fn clear_quotas(dataset: &str) -> Result<()> {
    set_size_property(dataset, "quota", None).await?;
    set_size_property(dataset, "refreservation", None).await?;
    Ok(())
}

/// Destroy one snapshot. Unlike [`destroy`] (which is recursive
/// and tolerates a missing dataset), this is strict: a missing
/// snapshot surfaces as an error so the saga's cleanup logs
/// catch typos / wrong names instead of silently moving on.
pub async fn destroy_snapshot(snapshot: &str) -> Result<()> {
    let output = Command::new("zfs")
        .args(["destroy", snapshot])
        .output()
        .await
        .with_context(|| format!("spawn zfs destroy {snapshot}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs destroy {snapshot} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    Ok(())
}

/// Parse one `zfs get -Hp -o value <prop> <dataset>` line.
///
/// zfs reports unset size properties as `0` (the size form) or
/// `none` (the symbolic form). With `-p` (parseable) you get the
/// number form, so `0` means unset. We surface that as `None` so
/// the saga's restore step knows whether to set the value back
/// or leave it cleared.
async fn get_size_property(dataset: &str, prop: &str) -> Result<Option<u64>> {
    let output = Command::new("zfs")
        .args(["get", "-Hp", "-o", "value", prop, dataset])
        .output()
        .await
        .with_context(|| format!("spawn zfs get {prop} {dataset}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs get {prop} {dataset} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0" || trimmed == "none" {
        return Ok(None);
    }
    let n: u64 = trimmed
        .parse()
        .with_context(|| format!("parse zfs get {prop} {dataset} output: {trimmed:?}"))?;
    Ok(Some(n))
}

/// Set `dataset`'s `prop` to `bytes` (`None` → `none`).
async fn set_size_property(dataset: &str, prop: &str, bytes: Option<u64>) -> Result<()> {
    let value = match bytes {
        Some(n) => n.to_string(),
        None => "none".to_string(),
    };
    let assignment = format!("{prop}={value}");
    let output = Command::new("zfs")
        .args(["set", &assignment, dataset])
        .output()
        .await
        .with_context(|| format!("spawn zfs set {assignment} {dataset}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs set {assignment} {dataset} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_quotas_default_is_both_none() {
        let q = SavedQuotas::default();
        assert!(q.quota_bytes.is_none());
        assert!(q.refreservation_bytes.is_none());
    }

    #[test]
    fn saved_quotas_copy_round_trips() {
        let q1 = SavedQuotas {
            quota_bytes: Some(1024 * 1024 * 1024),
            refreservation_bytes: Some(512 * 1024 * 1024),
        };
        let q2 = q1;
        assert_eq!(q1, q2);
    }
}
