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

/// Return the `volsize` property of a zvol dataset, in bytes.
/// Returns `Ok(None)` when the dataset is not a zvol (volsize is
/// not set on filesystem datasets). Used by image-manifest synth
/// for bhyve/kvm images so vmadm knows the boot-disk size.
pub async fn volsize_bytes(dataset: &str) -> Result<Option<u64>> {
    let output = Command::new("zfs")
        .args(["get", "-H", "-p", "-o", "value", "volsize", dataset])
        .output()
        .await
        .context("spawn zfs get volsize")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("zfs get volsize {dataset}: {stderr}"));
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Filesystem datasets show "-" for volsize; zvols show the
    // size in bytes (because of -p).
    if raw == "-" || raw.is_empty() {
        return Ok(None);
    }
    let n: u64 = raw
        .parse()
        .with_context(|| format!("zfs get volsize {dataset} returned non-numeric: {raw:?}"))?;
    Ok(Some(n))
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

/// Recursively destroy a dataset, forcing an unmount of any
/// mounted filesystems first (`-f`). The migration-target zone is
/// created `installed` by `vmadm create`, which mounts its zone
/// root, so a plain `zfs destroy` fails `Device busy`. The zone's
/// `zoneadm` state is unaffected (it stays `installed`), so the
/// later boot re-mounts the freshly received datasets.
pub async fn destroy_forced(dataset: &str) -> Result<()> {
    let output = Command::new("zfs")
        .args(["destroy", "-r", "-f", dataset])
        .output()
        .await
        .context("spawn zfs destroy -rf")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("does not exist") {
        return Ok(());
    }
    Err(anyhow!(
        "zfs destroy -rf {dataset} failed (exit {}): {stderr}",
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
/// type here to avoid an agent → store dependency arrow. The
/// serde field names double as the `QuotaDanceSaveResult` job
/// `result` contract and the on-dataset stash format, so renames
/// are wire changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SavedQuotas {
    /// `zfs get -Hp -o value quota <ds>` parsed as bytes. `None`
    /// when zfs returns `0` (i.e. the property was unset, which
    /// zfs reports as 0 — distinct from "explicit 0" which is
    /// invalid anyway).
    #[serde(default)]
    pub quota_bytes: Option<u64>,
    /// Same for `refreservation`.
    #[serde(default)]
    pub refreservation_bytes: Option<u64>,
}

/// Take a migration-tagged snapshot: `dataset@migration-{label}`.
///
/// Returns the fully-qualified snapshot name on success. The
/// label is appended verbatim so the caller can encode the
/// migration id + iteration ("base", "increment-1", "final",
/// "postpause") in a single human-readable spot.
///
/// Recursive (`-r`): a bhyve zone's boot/data zvols are child
/// datasets (`zones/<uuid>/disk0`), so a non-recursive snapshot
/// would silently drop every guest disk from the stream.
pub async fn snapshot_for_migration(dataset: &str, label: &str) -> Result<String> {
    let full = format!("{dataset}@migration-{label}");
    let args = migration_snapshot_args(&full);
    let output = Command::new("zfs")
        .args(&args)
        .output()
        .await
        .context("spawn zfs snapshot -r")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs snapshot -r {full} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    Ok(full)
}

fn migration_snapshot_args(snapshot: &str) -> Vec<String> {
    vec!["snapshot".into(), "-r".into(), snapshot.into()]
}

fn send_full_args(snapshot: &str) -> Vec<String> {
    // No `-R`: each dataset in the tree is sent on its own connection
    // as a self-contained (flattened) stream. A replication stream
    // would preserve the boot disk's clone-of-image relationship, and
    // the receive then fails on a CN whose image snapshot has a
    // different GUID ("local origin for clone ... does not exist").
    vec!["send".into(), "-w".into(), snapshot.into()]
}

fn send_incremental_args(from_snap: &str, to_snap: &str) -> Vec<String> {
    vec![
        "send".into(),
        "-w".into(),
        "-i".into(),
        from_snap.into(),
        to_snap.into(),
    ]
}

fn send_estimate_args(from_snap: Option<&str>, to_snap: &str) -> Vec<String> {
    // Mirror the real per-dataset send flags (-w [-i]) so the dry-run
    // sizes the exact stream we are about to produce; -n makes it a
    // no-op, -P asks for machine-parseable exact byte counts.
    let mut args: Vec<String> = vec!["send".into(), "-n".into(), "-P".into()];
    match from_snap {
        Some(from) => {
            args.extend(send_incremental_args(from, to_snap).into_iter().skip(1));
        }
        None => {
            args.extend(send_full_args(to_snap).into_iter().skip(1));
        }
    }
    args
}

fn recv_args(dataset: &str) -> Vec<String> {
    // Only `-x refreservation` (valid for both filesystems and
    // volumes): the boot/data disks are zvols, and `-x quota` errors
    // "property 'quota' does not apply to datasets of this type" on a
    // volume receive. The source's quota is already cleared by the
    // quota dance before the snapshot, so it never rides the stream;
    // the saga restores quota + refreservation after the cutover.
    vec![
        "recv".into(),
        "-F".into(),
        "-u".into(),
        "-x".into(),
        "refreservation".into(),
        dataset.into(),
    ]
}

fn list_snapshots_args(dataset: &str) -> Vec<String> {
    vec![
        "list".into(),
        "-H".into(),
        "-t".into(),
        "snapshot".into(),
        "-o".into(),
        "name".into(),
        "-d".into(),
        "1".into(),
        dataset.into(),
    ]
}

/// Spawn `zfs send -w -R <snap>` with stdout piped.
///
/// `-w` (raw, encryption-preserving) is unconditional so encrypted
/// migrations work once the placement filter that rejects them is
/// lifted; harmless on unencrypted datasets. `-R` (replication
/// stream) carries the whole dataset tree; without it a bhyve
/// zone's disk zvols never reach the target.
pub fn spawn_send_full(snapshot: &str) -> Result<Child> {
    let mut cmd = Command::new("zfs");
    cmd.args(send_full_args(snapshot))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    cmd.spawn()
        .with_context(|| format!("spawn zfs send {snapshot}"))
}

/// Spawn `zfs send -w -R -I <from> <to>` (incremental replication
/// stream) with stdout piped. Same lifetime contract as
/// [`spawn_send_full`].
///
/// `-I` (not `-i`): a replication stream must keep every dataset's
/// snapshot chain intact on the receiver, and `-I` ships all
/// intermediate snapshots between the two endpoints so the next
/// round's incremental base exists on every child dataset.
pub fn spawn_send_incremental(from_snap: &str, to_snap: &str) -> Result<Child> {
    let mut cmd = Command::new("zfs");
    cmd.args(send_incremental_args(from_snap, to_snap))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    cmd.spawn()
        .with_context(|| format!("spawn zfs send -I {from_snap} {to_snap}"))
}

/// Estimate the byte size of the stream [`spawn_send_full`] /
/// [`spawn_send_incremental`] would produce, via a `zfs send -n -P`
/// dry run with the same flags. Both snapshots must already exist.
/// The estimate feeds the migration progress reporter's
/// `total_progress`; callers should degrade to "no total" on error
/// rather than failing the transfer.
pub async fn estimate_send_bytes(from_snap: Option<&str>, to_snap: &str) -> Result<u64> {
    let output = Command::new("zfs")
        .args(send_estimate_args(from_snap, to_snap))
        .output()
        .await
        .with_context(|| format!("spawn zfs send -nP {to_snap}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs send -nP {to_snap} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    // illumos zfs prints the dry-run report on stderr; OpenZFS
    // moved it to stdout. Parse both rather than caring which.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_send_estimate(&stdout)
        .or_else(|| parse_send_estimate(&stderr))
        .ok_or_else(|| anyhow!("zfs send -nP {to_snap}: no parseable size line in output"))
}

/// Pull the total from `zfs send -n -P` output: per-stream
/// `full`/`incremental` lines followed by one `size <bytes>`
/// summary. With `-R` packages each sub-stream may emit its own
/// lines; the final `size` line is the package total, so the last
/// one wins.
fn parse_send_estimate(output: &str) -> Option<u64> {
    output.lines().rev().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("size") {
            return None;
        }
        fields.next()?.parse().ok()
    })
}

/// Spawn `zfs recv -F -u -x quota -x refreservation <dataset>`
/// with stdin piped.
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
/// activates it after the cutover. Excludes `quota` and
/// `refreservation` (`-x`) because the replication stream carries
/// the source's properties and a received quota smaller than the
/// in-flight stream aborts the receive; the saga's quota dance
/// restores both on whichever side ends up owning the dataset.
pub fn spawn_recv(dataset: &str) -> Result<Child> {
    let mut cmd = Command::new("zfs");
    cmd.args(recv_args(dataset))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd.spawn()
        .with_context(|| format!("spawn zfs recv {dataset}"))
}

/// List the filesystems and volumes in a dataset tree, parent first
/// (the order `zfs list -r` returns). The migration sends each as its
/// own flattened stream, so the zone root must precede its child disk
/// zvols for every receive's parent to already exist on the target.
pub async fn list_migration_tree(dataset: &str) -> Result<Vec<String>> {
    let output = Command::new("zfs")
        .args([
            "list",
            "-H",
            "-o",
            "name",
            "-r",
            "-t",
            "filesystem,volume",
            dataset,
        ])
        .output()
        .await
        .with_context(|| format!("zfs list -r {dataset}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs list -r {dataset} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// List the names of `dataset`'s migration snapshots (the
/// `@migration-*` family the saga creates). Depth-limited to the
/// dataset itself: the snapshots are created with `-r`, so
/// destroying the parent's snapshot with `-r` takes the
/// children's along, and listing descendants would only produce
/// names that vanish mid-cleanup.
///
/// A dataset that does not exist returns an empty list: cleanup
/// runs after `vmadm delete`, which usually destroys the whole
/// dataset tree first.
pub async fn list_migration_snapshots(dataset: &str) -> Result<Vec<String>> {
    let output = Command::new("zfs")
        .args(list_snapshots_args(dataset))
        .output()
        .await
        .context("spawn zfs list -t snapshot")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not exist") {
            return Ok(Vec::new());
        }
        return Err(anyhow!(
            "zfs list -t snapshot {dataset} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_migration_snapshots(&stdout))
}

/// Filter a `zfs list -H -o name` listing down to migration
/// snapshots (name part after `@` starts with `migration-`).
fn parse_migration_snapshots(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            line.split_once('@')
                .is_some_and(|(_, snap)| snap.starts_with("migration-"))
        })
        .map(str::to_string)
        .collect()
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

/// Destroy one snapshot across the dataset tree (`destroy -r` of
/// a snapshot name takes the same-named snapshot on every child,
/// the mirror image of the `-r` create in
/// [`snapshot_for_migration`]). Unlike [`destroy`] this is strict
/// on a missing snapshot: the saga's cleanup logs should catch
/// typos / wrong names instead of silently moving on.
pub async fn destroy_snapshot(snapshot: &str) -> Result<()> {
    let output = Command::new("zfs")
        .args(["destroy", "-r", snapshot])
        .output()
        .await
        .with_context(|| format!("spawn zfs destroy {snapshot}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs destroy -r {snapshot} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    Ok(())
}

/// ZFS user property the quota dance stashes the original
/// `quota`/`refreservation` under (JSON-encoded [`SavedQuotas`]).
/// Living on the dataset itself makes `SaveAndClear` idempotent
/// across agent crashes: a re-claimed job whose clear already ran
/// reads the stash instead of reporting the cleared (None)
/// values, which would silently lose the tenant's quota.
const MIGRATION_QUOTA_STASH_PROP: &str = "tritond:migration_saved_quotas";

/// Persist `saved` on the dataset (see
/// [`MIGRATION_QUOTA_STASH_PROP`]). Must run BEFORE
/// [`clear_quotas`] so a crash between the two never loses the
/// originals.
pub async fn stash_saved_quotas(dataset: &str, saved: SavedQuotas) -> Result<()> {
    let encoded = serde_json::to_string(&saved).context("encode SavedQuotas stash")?;
    let assignment = format!("{MIGRATION_QUOTA_STASH_PROP}={encoded}");
    let output = Command::new("zfs")
        .args(["set", &assignment, dataset])
        .output()
        .await
        .with_context(|| format!("spawn zfs set quota stash on {dataset}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs set quota stash on {dataset} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    Ok(())
}

/// Read back a previously stashed [`SavedQuotas`], `Ok(None)` when
/// no stash is set (zfs reports `-` for an unset user property).
pub async fn read_stashed_quotas(dataset: &str) -> Result<Option<SavedQuotas>> {
    let output = Command::new("zfs")
        .args([
            "get",
            "-H",
            "-o",
            "value",
            MIGRATION_QUOTA_STASH_PROP,
            dataset,
        ])
        .output()
        .await
        .with_context(|| format!("spawn zfs get quota stash on {dataset}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs get quota stash on {dataset} failed (exit {}): {stderr}",
            output.status,
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "-" {
        return Ok(None);
    }
    let saved: SavedQuotas = serde_json::from_str(trimmed)
        .with_context(|| format!("parse quota stash on {dataset}: {trimmed:?}"))?;
    Ok(Some(saved))
}

/// Drop the quota stash (`zfs inherit` of a user property removes
/// the local value; inheriting a property that was never set is a
/// no-op, so this is idempotent and safe on the target side where
/// the stash never existed).
pub async fn clear_stashed_quotas(dataset: &str) -> Result<()> {
    let output = Command::new("zfs")
        .args(["inherit", MIGRATION_QUOTA_STASH_PROP, dataset])
        .output()
        .await
        .with_context(|| format!("spawn zfs inherit quota stash on {dataset}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "zfs inherit quota stash on {dataset} failed (exit {}): {stderr}",
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

    #[test]
    fn saved_quotas_json_round_trips_and_tolerates_missing_fields() {
        let q = SavedQuotas {
            quota_bytes: Some(42),
            refreservation_bytes: None,
        };
        let json = serde_json::to_string(&q).unwrap();
        let back: SavedQuotas = serde_json::from_str(&json).unwrap();
        assert_eq!(q, back);
        // A stash written before a field existed must still parse.
        let sparse: SavedQuotas = serde_json::from_str("{}").unwrap();
        assert_eq!(sparse, SavedQuotas::default());
    }

    #[test]
    fn migration_snapshot_args_are_recursive() {
        assert_eq!(
            migration_snapshot_args("zones/abc@migration-base"),
            vec!["snapshot", "-r", "zones/abc@migration-base"],
        );
    }

    #[test]
    fn send_full_args_form_a_raw_replication_stream() {
        assert_eq!(
            send_full_args("zones/abc@migration-base"),
            vec!["send", "-w", "-R", "zones/abc@migration-base"],
        );
    }

    #[test]
    fn send_incremental_args_keep_intermediate_snapshots() {
        assert_eq!(
            send_incremental_args("zones/abc@migration-base", "zones/abc@migration-sync-1"),
            vec![
                "send",
                "-w",
                "-R",
                "-I",
                "zones/abc@migration-base",
                "zones/abc@migration-sync-1",
            ],
        );
    }

    #[test]
    fn send_estimate_args_mirror_full_send_flags() {
        assert_eq!(
            send_estimate_args(None, "zones/abc@migration-base"),
            vec!["send", "-n", "-P", "-w", "-R", "zones/abc@migration-base"],
        );
    }

    #[test]
    fn send_estimate_args_mirror_incremental_send_flags() {
        assert_eq!(
            send_estimate_args(
                Some("zones/abc@migration-base"),
                "zones/abc@migration-sync-1"
            ),
            vec![
                "send",
                "-n",
                "-P",
                "-w",
                "-R",
                "-I",
                "zones/abc@migration-base",
                "zones/abc@migration-sync-1",
            ],
        );
    }

    #[test]
    fn parse_send_estimate_reads_full_dry_run() {
        let out = "\
full\tzones/abc@migration-base\t10737418240
size\t10737418240
";
        assert_eq!(parse_send_estimate(out), Some(10_737_418_240));
    }

    #[test]
    fn parse_send_estimate_takes_package_total_from_replication_stream() {
        // -R packages emit per-substream lines; the final size line
        // is the package total and must win.
        let out = "\
incremental\tmigration-base\tzones/abc@migration-sync-1\t1048576
size\t1048576
incremental\tmigration-base\tzones/abc/disk0@migration-sync-1\t52428800
size\t52428800
size\t53477376
";
        assert_eq!(parse_send_estimate(out), Some(53_477_376));
    }

    #[test]
    fn parse_send_estimate_tolerates_space_separated_fields() {
        assert_eq!(parse_send_estimate("size 4096\n"), Some(4096));
    }

    #[test]
    fn parse_send_estimate_rejects_garbage() {
        assert_eq!(parse_send_estimate(""), None);
        assert_eq!(
            parse_send_estimate("cannot open 'zones/abc': dataset does not exist\n"),
            None,
        );
        // A human-form (-v without -P) line must not half-parse.
        assert_eq!(parse_send_estimate("total estimated size is 1.52G\n"), None);
        assert_eq!(parse_send_estimate("size\tnot-a-number\n"), None);
    }

    #[test]
    fn recv_args_exclude_quota_properties() {
        assert_eq!(
            recv_args("zones/abc"),
            vec![
                "recv",
                "-F",
                "-u",
                "-x",
                "quota",
                "-x",
                "refreservation",
                "zones/abc",
            ],
        );
    }

    #[test]
    fn list_snapshots_args_are_depth_limited_to_the_dataset() {
        assert_eq!(
            list_snapshots_args("zones/abc"),
            vec![
                "list",
                "-H",
                "-t",
                "snapshot",
                "-o",
                "name",
                "-d",
                "1",
                "zones/abc",
            ],
        );
    }

    #[test]
    fn parse_migration_snapshots_filters_the_migration_family() {
        let stdout = "\
zones/abc@migration-base
zones/abc@migration-sync-1
zones/abc@migration-final
zones/abc@daily-2026-06-09
zones/abc@final

  zones/abc@migration-sync-2
";
        assert_eq!(
            parse_migration_snapshots(stdout),
            vec![
                "zones/abc@migration-base",
                "zones/abc@migration-sync-1",
                "zones/abc@migration-final",
                "zones/abc@migration-sync-2",
            ],
        );
    }

    #[test]
    fn parse_migration_snapshots_ignores_non_snapshot_lines() {
        // A dataset named to look like the family but with no `@`
        // must not slip through to a `zfs destroy -r`.
        assert!(parse_migration_snapshots("zones/migration-base\n").is_empty());
        assert!(parse_migration_snapshots("").is_empty());
    }
}
