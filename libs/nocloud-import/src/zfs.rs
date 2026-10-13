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
    let args = create_zvol_args(dataset, size_mib);
    let mut argv = vec!["zfs"];
    argv.extend(args.iter().map(String::as_str));
    run(&argv).await
}

/// `zfs` arguments for the build zvol. It is sparse (`-s`): the image only
/// fills what it writes, and the zvol is destroyed after the build, so
/// reserving its whole virtual size would only make imports fail on pools
/// with less free space than that.
fn create_zvol_args(dataset: &str, size_mib: u64) -> Vec<String> {
    ["create", "-s", "-V", &format!("{size_mib}m"), dataset]
        .map(str::to_string)
        .to_vec()
}

pub async fn snap(snap_spec: &str) -> Result<()> {
    run(&["zfs", "snap", snap_spec]).await
}

pub async fn send_to_file(snap_spec: &str, out: &Path) -> Result<()> {
    let out_file = tokio::fs::File::create(out)
        .await
        .with_context(|| format!("create {}", out.display()))?
        .into_std()
        .await;
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
    // Cleanup also runs from the failure path of a build; the caller
    // is expected to log (not bail) so the outer error surfaces.
    let status = Command::new("zfs")
        .args(["destroy", "-r", dataset])
        .status()
        .await
        .with_context(|| format!("spawn zfs destroy {dataset}"))?;
    if !status.success() {
        bail!("zfs destroy -r {dataset} exited {status}");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The build zvol is sparse: a large virtual size does not reserve
    /// that much pool space up front, only what the image writes.
    #[test]
    fn the_build_zvol_is_sparse() {
        assert_eq!(
            create_zvol_args("zones/build", 10240),
            ["create", "-s", "-V", "10240m", "zones/build"]
        );
    }
}
