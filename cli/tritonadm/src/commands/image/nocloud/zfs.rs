// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrappers around the `zfs(8)` CLI. Assumes the caller has
//! sufficient privileges (running as root in the GZ, or with the
//! `Primary Administrator` profile via `pfexec` in an NGZ).

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

pub async fn create_zvol(dataset: &str, size_mib: u64) -> Result<()> {
    let size = format!("{size_mib}m");
    run(&["zfs", "create", "-V", &size, dataset]).await
}

pub async fn snap(snap_spec: &str) -> Result<()> {
    run(&["zfs", "snap", snap_spec]).await
}

pub async fn send_to_file(snap_spec: &str, out: &Path) -> Result<()> {
    let out_file = std::fs::File::create(out)
        .with_context(|| format!("create {}", out.display()))?;
    let status = Command::new("zfs")
        .args(["send", snap_spec])
        .stdout(Stdio::from(out_file))
        .status()
        .await
        .context("spawn zfs send")?;
    if !status.success() {
        bail!("zfs send {snap_spec} exited {status}");
    }
    Ok(())
}

pub async fn destroy_recursive(dataset: &str) -> Result<()> {
    // Best-effort cleanup. Errors are swallowed because this also runs
    // from the failure path of a build, where the outer error is what
    // we want to surface.
    let _ = Command::new("zfs")
        .args(["destroy", "-r", dataset])
        .status()
        .await;
    Ok(())
}

/// List immediate children of `parent` whose names match
/// `<parent>/<prefix>...`. Returns full dataset names. Used for
/// finding leftover datasets from a previous interrupted build.
pub async fn list_children_with_prefix(parent: &str, prefix: &str) -> Result<Vec<String>> {
    let out = Command::new("zfs")
        .args(["list", "-H", "-o", "name", "-d", "1", parent])
        .output()
        .await
        .with_context(|| format!("spawn zfs list {parent}"))?;
    if !out.status.success() {
        bail!(
            "zfs list -d 1 {parent} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let prefix_full = format!("{parent}/{prefix}");
    Ok(body
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(&prefix_full))
        .map(String::from)
        .collect())
}

/// Read the dataset's creation epoch (seconds since 1970-01-01 UTC).
/// `zfs get -p` returns the raw seconds rather than a formatted date.
pub async fn get_creation_epoch(dataset: &str) -> Result<u64> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-p", "-o", "value", "creation", dataset])
        .output()
        .await
        .with_context(|| format!("spawn zfs get creation {dataset}"))?;
    if !out.status.success() {
        bail!(
            "zfs get creation {dataset} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim()
        .parse::<u64>()
        .with_context(|| format!("parse zfs creation epoch from {s:?}"))
}

/// Attempt to destroy a dataset; returns `Ok(true)` on success,
/// `Ok(false)` if zfs refused (busy, missing, etc.). Used by the
/// sweep, which wants to distinguish "destroyed stale leftover" from
/// "skipped because another build is using it."
pub async fn try_destroy_recursive(dataset: &str) -> Result<bool> {
    let status = Command::new("zfs")
        .args(["destroy", "-r", dataset])
        .stderr(Stdio::null())
        .status()
        .await
        .with_context(|| format!("spawn zfs destroy {dataset}"))?;
    Ok(status.success())
}

async fn run(args: &[&str]) -> Result<()> {
    let (cmd, rest) = args
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;
    let status = Command::new(cmd)
        .args(rest)
        .status()
        .await
        .with_context(|| format!("spawn {args:?}"))?;
    if !status.success() {
        bail!("{args:?} exited {status}");
    }
    Ok(())
}
