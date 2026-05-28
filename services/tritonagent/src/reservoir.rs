// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Agent-side management of the bhyve memory reservoir (RFD 0185).
//!
//! The reservoir is a host-level pool of physical memory bhyve guests
//! draw from instead of racing the kernel for transient pages. It is
//! **not persistent across reboot**, so the agent re-establishes a target
//! on every startup. The management model (per the reservoir plan) is a
//! static floor plus grow-on-demand: at startup we set the reservoir to a
//! floor sized as a fraction of physical RAM, and later (RV-3) grow it as
//! reservoir-backed guests are provisioned.
//!
//! Sizing is best-effort: a host already running transient guests may not
//! be able to free enough memory to reach the floor immediately. We apply
//! what we can, log the shortfall, and re-converge on the next apply.

use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};
use tritond_cn_platform::smartos::reservoir::ReservoirState;
use tritond_cn_platform::smartos::{KstatTool, ReservoirTool};

const BYTES_PER_MIB: u64 = 1024 * 1024;

/// Owns the host reservoir target. Holds the shared [`ReservoirTool`] (so
/// its `/dev/vmmctl` lock is shared with the status collector) plus a
/// [`KstatTool`] to read physical memory.
pub struct ReservoirManager {
    tool: Arc<ReservoirTool>,
    kstat: Arc<KstatTool>,
}

impl ReservoirManager {
    pub fn new(tool: Arc<ReservoirTool>, kstat: Arc<KstatTool>) -> Self {
        Self { tool, kstat }
    }

    /// Current reservoir sizing.
    pub async fn state(&self) -> Result<ReservoirState> {
        Ok(self.tool.query().await?)
    }

    /// Set the reservoir floor to `percent` of physical RAM, clamped to the
    /// kernel's reservoir limit. Idempotent: a no-op when already at the
    /// computed target. Returns the achieved state.
    pub async fn apply_floor(&self, percent: f32) -> Result<ReservoirState> {
        let percent = percent.clamp(0.0, 1.0) as f64;
        let state = self.tool.query().await?;
        let total_mib = self.kstat.memory_info().await?.total_bytes / BYTES_PER_MIB;

        let target = floor_target_mib(total_mib, percent, state.limit_mib);

        if state.current_mib() == target {
            info!(target_mib = target, "reservoir already at floor; no resize");
            return Ok(state);
        }

        info!(
            current_mib = state.current_mib(),
            target_mib = target,
            total_mib,
            limit_mib = state.limit_mib,
            percent,
            "applying reservoir floor",
        );
        let achieved = self.tool.set_target(target).await?;
        if achieved.current_mib() < target {
            warn!(
                target_mib = target,
                achieved_mib = achieved.current_mib(),
                "reservoir floor not fully reached (insufficient free memory); will re-converge on next apply",
            );
        }
        Ok(achieved)
    }
}

impl ReservoirManager {
    /// Ensure at least `requested_mib` is free in the reservoir, growing
    /// toward the kernel limit if needed (grow-on-demand). Returns the
    /// free MiB actually available afterward — which may be less than
    /// requested if the host is at its reservoir limit, in which case the
    /// caller must reject the provision.
    pub async fn ensure_free(&self, requested_mib: u64) -> Result<u64> {
        let st = self.tool.query().await?;
        if st.free_mib >= requested_mib {
            return Ok(st.free_mib);
        }
        let deficit = requested_mib - st.free_mib;
        let target = st.current_mib().saturating_add(deficit).min(st.limit_mib);
        let achieved = self.tool.set_target(target).await?;
        Ok(achieved.free_mib)
    }
}

/// The effective reservoir policy + manager, resolved once at startup and
/// consulted on each bhyve provision. `None` is threaded through the
/// provision path when the agent isn't managing a reservoir.
pub struct ReservoirRuntime {
    pub enabled: bool,
    pub percent: f32,
    manager: ReservoirManager,
}

impl ReservoirRuntime {
    pub fn new(enabled: bool, percent: f32, manager: ReservoirManager) -> Self {
        Self {
            enabled,
            percent,
            manager,
        }
    }

    /// Grow the reservoir so a `requested_mib` bhyve guest fits before it
    /// is created. The kernel does NOT fall back to transient memory, so a
    /// guest that can't be backed must not be created: this returns `Err`
    /// (the provision is failed) when the host is at reservoir capacity.
    pub async fn ensure_capacity(&self, requested_mib: u64) -> Result<()> {
        let free = self.manager.ensure_free(requested_mib).await?;
        if free < requested_mib {
            anyhow::bail!(
                "reservoir at capacity: need {requested_mib} MiB for this guest but only \
                 {free} MiB free after growing to the kernel limit"
            );
        }
        Ok(())
    }
}

/// Compute the floor target in MiB: `percent` of physical RAM, never above
/// the kernel-enforced reservoir `limit_mib`.
fn floor_target_mib(total_mib: u64, percent: f64, limit_mib: u64) -> u64 {
    let want = (total_mib as f64 * percent) as u64;
    want.min(limit_mib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_is_percent_of_ram() {
        // 80% of 98_208 MiB = 78_566 MiB, well under the limit.
        assert_eq!(floor_target_mib(98_208, 0.80, 93_639), 78_566);
    }

    #[test]
    fn floor_clamps_to_limit() {
        // 100% of RAM exceeds the kernel limit; clamp to it. (On real
        // hardware the limit sits near ~95% of physmem, so only a high
        // percent clamps.)
        assert_eq!(floor_target_mib(98_208, 1.0, 93_639), 93_639);
        // A low kernel limit clamps even a modest percent.
        assert_eq!(floor_target_mib(98_208, 0.50, 10_000), 10_000);
    }

    #[test]
    fn zero_percent_is_zero() {
        assert_eq!(floor_target_mib(98_208, 0.0, 93_639), 0);
    }
}
