// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Pre-upgrade validation: cluster health, etcd quorum, version compatibility.

use anyhow::{Context, Result, bail};

use super::super::state::ClusterState;
use super::super::talos;
use super::discovery::Plan;

/// Run every preflight check; returns Err on any failure (caller may bypass
/// with `--force`).
pub async fn run(
    plan: &Plan,
    state: &ClusterState,
    talosconfig: &std::path::Path,
    health_timeout: &str,
    json: bool,
) -> Result<()> {
    if !json {
        eprintln!("==> Running preflight checks");
    }

    check_version_compatibility(plan)?;
    if !json {
        eprintln!(
            "    Version compatibility: OK ({} -> {})",
            plan.from_version, plan.to_version
        );
    }

    check_etcd_quorum_capacity(plan)?;
    if !json {
        eprintln!(
            "    etcd quorum capacity: OK ({} control plane nodes)",
            plan.control_plane.len()
        );
    }

    check_cluster_health(plan, state, talosconfig, health_timeout, json).await?;
    if !json {
        eprintln!("    Cluster health: OK");
        eprintln!();
    }

    Ok(())
}

/// A minor version may upgrade to its current or the immediately following
/// minor (matches what `talosctl upgrade-k8s` enforces via
/// `kubernetes/upgrade.NewPath`). Patch upgrades within the same minor are
/// always allowed; downgrades are rejected.
fn check_version_compatibility(plan: &Plan) -> Result<()> {
    let from = parse_semver(&plan.from_version)
        .with_context(|| format!("could not parse current version '{}'", plan.from_version))?;
    let to = parse_semver(&plan.to_version)
        .with_context(|| format!("could not parse target version '{}'", plan.to_version))?;

    if to < from {
        bail!(
            "downgrade not supported: {} -> {} (use --force to override)",
            plan.from_version,
            plan.to_version
        );
    }

    let minor_jump = to.minor as i64 - from.minor as i64;
    if to.major != from.major || minor_jump > 1 {
        bail!(
            "unsupported version skew {} -> {}: only same-minor patches \
             or single-minor upgrades are permitted (use --force to override)",
            plan.from_version,
            plan.to_version
        );
    }

    Ok(())
}

/// Refuse to start an upgrade if etcd quorum is too tight to survive a
/// one-node outage. With N control plane nodes, quorum = floor(N/2)+1; we
/// require N >= 3 (matches Talos behavior, which also refuses 2-node CP
/// upgrades server-side).
fn check_etcd_quorum_capacity(plan: &Plan) -> Result<()> {
    let n = plan.control_plane.len();
    if n < 3 {
        bail!(
            "control plane has {} node(s); kubelet/static-pod roll requires \
             at least 3 to maintain etcd quorum during the rolling restart \
             (use --force to override)",
            n
        );
    }
    Ok(())
}

async fn check_cluster_health(
    plan: &Plan,
    _state: &ClusterState,
    talosconfig: &std::path::Path,
    health_timeout: &str,
    json: bool,
) -> Result<()> {
    let endpoint = &plan
        .control_plane
        .first()
        .context("plan has no control plane node")?
        .talos_endpoint;
    if !json {
        eprintln!(
            "    Checking cluster health (timeout: {})...",
            health_timeout
        );
    }
    talos::health::run(
        endpoint,
        health_timeout,
        Some(&talosconfig.to_string_lossy()),
        None,
        false,
    )
    .await
    .context("Talos cluster health check failed")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SemVer {
    major: u32,
    minor: u32,
    patch: u32,
}

fn parse_semver(s: &str) -> Result<SemVer> {
    let s = s.strip_prefix('v').unwrap_or(s);
    // Drop any pre-release / build suffix.
    let core = s.split(|c: char| c == '-' || c == '+').next().unwrap_or(s);
    let mut parts = core.split('.');
    let major = parts
        .next()
        .context("missing major")?
        .parse::<u32>()
        .context("non-numeric major")?;
    let minor = parts
        .next()
        .context("missing minor")?
        .parse::<u32>()
        .context("non-numeric minor")?;
    let patch = parts
        .next()
        .unwrap_or("0")
        .parse::<u32>()
        .context("non-numeric patch")?;
    Ok(SemVer {
        major,
        minor,
        patch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_versions() {
        assert_eq!(parse_semver("v1.36.0").unwrap().minor, 36);
        assert_eq!(parse_semver("1.36.0").unwrap().minor, 36);
        assert_eq!(parse_semver("v1.36").unwrap().patch, 0);
        assert_eq!(parse_semver("v1.36.0-rc.1").unwrap().minor, 36);
    }

    #[test]
    fn allows_patch_and_single_minor() {
        let plan_ok_patch = test_plan("v1.36.0", "v1.36.5");
        let plan_ok_minor = test_plan("v1.35.0", "v1.36.0");
        assert!(check_version_compatibility(&plan_ok_patch).is_ok());
        assert!(check_version_compatibility(&plan_ok_minor).is_ok());
    }

    #[test]
    fn rejects_big_skew_and_downgrade() {
        let plan_skip = test_plan("v1.34.0", "v1.36.0");
        let plan_back = test_plan("v1.36.0", "v1.35.0");
        assert!(check_version_compatibility(&plan_skip).is_err());
        assert!(check_version_compatibility(&plan_back).is_err());
    }

    fn test_plan(from: &str, to: &str) -> Plan {
        Plan {
            cluster_name: "x".into(),
            from_version: from.into(),
            to_version: to.into(),
            control_plane: vec![],
            workers: vec![],
        }
    }
}
