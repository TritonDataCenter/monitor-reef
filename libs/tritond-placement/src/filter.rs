// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Built-in [`crate::Filter`] implementations (RFD 00005 doc 02
//! §"The seventeen built-in filters").
//!
//! Each filter is a unit struct that implements [`Filter`]; they
//! are pure functions over [`CnView`], [`PlacementRequest`], and
//! [`ChainContext`]. Filters are kebab-case-named to match the
//! RFD; the names also flow through the FDB-backed cluster-
//! settings chain (PL-7).
//!
//! The opt-in `cn-load-not-overheating` guardrail lives at the
//! end. It is shipped but is *not* in [`default_chain`] -- the
//! operator opts in via cluster settings (D-Pl-9).
//!
//! ## Force-place
//!
//! Per D-Pl-8 the chain runner restricts the chain to
//! `cn-scope-match` only on the force-place path; the filters
//! here therefore do not have to special-case `req.force_cn`
//! themselves except for the two the RFD's table explicitly
//! calls out:
//!
//! * `cn-not-reserved` -- the runner already skips it on force
//!   path; the filter would also accept if it did run, since
//!   "operator out-of-service" is what force-place is *for*.
//! * `cn-scope-match` -- runs on the force path. Bypassed only
//!   if `req.ignore_scope_pin && req.force_cn ==
//!   Some(cn.server_uuid)`; the filter checks the combined flag
//!   here so the bypass cannot be triggered without an
//!   accompanying force-place.

use std::sync::Arc;

use crate::types::{ChainContext, CnStateView, CnView, Filter, PlacementRequest, Verdict};
use tritond_store::{AffinityOp, AffinityScope, AffinitySelector};

// ---------------------------------------------------------------------------
// 1. cn-approved-and-live
// ---------------------------------------------------------------------------

pub struct CnApprovedAndLive;
impl Filter for CnApprovedAndLive {
    fn name(&self) -> &'static str {
        "cn-approved-and-live"
    }
    fn evaluate(&self, cn: &CnView, _req: &PlacementRequest, ctx: &ChainContext) -> Verdict {
        if !matches!(cn.state, CnStateView::Approved) {
            return Verdict::reject(format!("cn-state {:?} is not Approved", cn.state));
        }
        match cn.last_seen {
            None => Verdict::reject("cn-last-seen is None (agent has never reported)"),
            Some(last) => {
                let age = ctx.now.signed_duration_since(last);
                let secs = age.num_seconds().max(0) as u64;
                if secs > ctx.agent_heartbeat_threshold_secs {
                    Verdict::reject(format!(
                        "agent heartbeat stale: last_seen={secs}s ago (threshold {}s)",
                        ctx.agent_heartbeat_threshold_secs
                    ))
                } else {
                    Verdict::Accept
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. cn-capacity-present
//
// Not in the RFD's 17-filter table per se, but RFD invariant 7
// requires that a CN with no `CnCapacity` row is invisible to
// placement -- every filter rejects it with reason "cn-capacity
// row absent". Implementing it as one explicit filter keeps the
// other capacity-reading filters from having to repeat the same
// `match cn.capacity { Some(c) => ..., None => Verdict::reject(...) }`
// pattern. It is the second entry in the default chain so the
// remaining capacity-reading filters can `unwrap` `cn.capacity`
// safely on the accepted path.
// ---------------------------------------------------------------------------

pub struct CnCapacityPresent;
impl Filter for CnCapacityPresent {
    fn name(&self) -> &'static str {
        "cn-capacity-present"
    }
    fn evaluate(&self, cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if cn.capacity.is_none() {
            Verdict::reject("cn-capacity row absent (agent has not reported structured capacity)")
        } else {
            Verdict::Accept
        }
    }
}

// ---------------------------------------------------------------------------
// 3. cn-role-matches
// ---------------------------------------------------------------------------

pub struct CnRoleMatches;
impl Filter for CnRoleMatches {
    fn name(&self) -> &'static str {
        "cn-role-matches"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if cn.role.satisfies(req.role) {
            Verdict::Accept
        } else {
            Verdict::reject(format!(
                "cn-role {:?} does not satisfy request role {:?}",
                cn.role, req.role
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// 4. cn-not-reserved
// ---------------------------------------------------------------------------

pub struct CnNotReserved;
impl Filter for CnNotReserved {
    fn name(&self) -> &'static str {
        "cn-not-reserved"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if !cn.placement.reserved {
            return Verdict::Accept;
        }
        // Operator-out-of-service flag. Force-place still works
        // when the same CN is the explicit target; the chain runner
        // additionally bypasses this filter on the force path
        // (D-Pl-8), so reaching this branch with a matching force_cn
        // is benign.
        if req.force_cn == Some(cn.server_uuid) {
            return Verdict::Accept;
        }
        Verdict::reject("cn-placement.reserved=true (operator out of service)")
    }
}

// ---------------------------------------------------------------------------
// 5. cn-not-cordoned
// ---------------------------------------------------------------------------

pub struct CnNotCordoned;
impl Filter for CnNotCordoned {
    fn name(&self) -> &'static str {
        "cn-not-cordoned"
    }
    fn evaluate(&self, cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if cn.placement.cordoned {
            let reason = cn
                .placement
                .cordoned_reason
                .as_deref()
                .unwrap_or("unspecified");
            Verdict::reject(format!("cn-placement.cordoned=true ({reason})"))
        } else {
            Verdict::Accept
        }
    }
}

// ---------------------------------------------------------------------------
// 6. cn-scope-match (D-Pl-5)
// ---------------------------------------------------------------------------

pub struct CnScopeMatch;
impl Filter for CnScopeMatch {
    fn name(&self) -> &'static str {
        "cn-scope-match"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        // Bypass: ignore_scope_pin only honoured together with a
        // force_cn pointing at *this* CN. An ignore_scope_pin
        // without an accompanying force is a programming error
        // (rejected at the API edge by PL-7); the runner's check
        // here is defence-in-depth.
        if req.ignore_scope_pin && req.force_cn == Some(cn.server_uuid) {
            return Verdict::Accept;
        }
        if let Some(silo) = cn.placement.pinned_silo_uuid
            && silo != req.silo_uuid
        {
            return Verdict::reject(format!(
                "cn pinned to silo {silo}, request silo is {}",
                req.silo_uuid
            ));
        }
        if let Some(tenant) = cn.placement.pinned_tenant_uuid
            && tenant != req.tenant_uuid
        {
            return Verdict::reject(format!(
                "cn pinned to tenant {tenant}, request tenant is {}",
                req.tenant_uuid
            ));
        }
        Verdict::Accept
    }
}

// ---------------------------------------------------------------------------
// 7. cn-platform-min
// ---------------------------------------------------------------------------

pub struct CnPlatformMin;
impl Filter for CnPlatformMin {
    fn name(&self) -> &'static str {
        "cn-platform-min"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        let Some(min) = req.min_platform.as_deref() else {
            return Verdict::Skip;
        };
        let Some(cap) = cn.capacity.as_ref() else {
            // cn-capacity-present already rejected; this filter
            // would too, but the explain report stays cleaner if
            // we skip rather than double-reject.
            return Verdict::Skip;
        };
        if cap.platform_version.as_str() < min {
            Verdict::reject(format!(
                "platform_version {} is below min_platform {min}",
                cap.platform_version
            ))
        } else {
            Verdict::Accept
        }
    }
}

// ---------------------------------------------------------------------------
// 8. cn-traits-required
// ---------------------------------------------------------------------------

pub struct CnTraitsRequired;
impl Filter for CnTraitsRequired {
    fn name(&self) -> &'static str {
        "cn-traits-required"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if req.required_traits.is_empty() {
            return Verdict::Skip;
        }
        for (key, want) in &req.required_traits {
            match cn.placement.traits.get(key) {
                None => return Verdict::reject(format!("trait `{key}` missing")),
                Some(have) if have != want => {
                    return Verdict::reject(format!(
                        "trait `{key}` is `{have}`, request wants `{want}`"
                    ));
                }
                Some(_) => {}
            }
        }
        Verdict::Accept
    }
}

// ---------------------------------------------------------------------------
// 9. cn-nic-tags
// ---------------------------------------------------------------------------

pub struct CnNicTags;
impl Filter for CnNicTags {
    fn name(&self) -> &'static str {
        "cn-nic-tags"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if req.required_nic_tags.is_empty() {
            return Verdict::Skip;
        }
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        for tag in &req.required_nic_tags {
            if !cap.nic_tags.iter().any(|have| have == tag) {
                return Verdict::reject(format!("nic-tag `{tag}` not advertised by this CN"));
            }
        }
        Verdict::Accept
    }
}

// ---------------------------------------------------------------------------
// 10. cn-underlay
// ---------------------------------------------------------------------------

pub struct CnUnderlay;
impl Filter for CnUnderlay {
    fn name(&self) -> &'static str {
        "cn-underlay"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        if cap.underlay.satisfies(req.required_underlay) {
            Verdict::Accept
        } else {
            Verdict::reject(format!(
                "underlay {:?} does not satisfy required {:?}",
                cap.underlay, req.required_underlay
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// 11. cn-zpool-has-space
//
// Per-pool, not aggregate. A request that names a specific pool
// must hit that pool; the "any pool with N bytes" shape (a
// request with one entry keyed by empty string) is handled by
// the package layer, not here. PL-1 of PL-3 implements only the
// per-pool semantics; package-driven "any pool" is a PL-7
// follow-up.
// ---------------------------------------------------------------------------

pub struct CnZpoolHasSpace;
impl Filter for CnZpoolHasSpace {
    fn name(&self) -> &'static str {
        "cn-zpool-has-space"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> Verdict {
        if req.disk.is_empty() {
            return Verdict::Skip;
        }
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        let overprov = cn
            .placement
            .overprovision_disk()
            .unwrap_or(ctx.cluster_overprovision.disk);
        // Sum per-pool reservation pressure (in bytes) from
        // active reservations.
        let mut reserved_per_pool: std::collections::BTreeMap<&str, u64> =
            std::collections::BTreeMap::new();
        for r in &cn.active_reservations {
            for (pool, bytes) in &r.disk {
                *reserved_per_pool.entry(pool.as_str()).or_insert(0) += *bytes;
            }
        }
        for (pool, want) in &req.disk {
            let Some(zpool) = cap.zpools.iter().find(|z| &z.name == pool) else {
                return Verdict::reject(format!("zpool `{pool}` not present on this CN"));
            };
            let reserved = reserved_per_pool.get(pool.as_str()).copied().unwrap_or(0);
            // Multiplier: 1.0 = no oversubscription, >1.0 =
            // overcommit. `free_bytes` is the agent-published
            // actual free (already net of running zones).
            let effective_free = ((zpool.free_bytes as f64) * overprov as f64).floor() as u64;
            let residual = effective_free.saturating_sub(reserved);
            if *want > residual {
                return Verdict::reject(format!(
                    "zpool `{pool}` needs {want} B, residual {residual} B \
                     (free={} reserved={reserved} overprov={overprov:.2})",
                    zpool.free_bytes
                ));
            }
        }
        Verdict::Accept
    }
}

// ---------------------------------------------------------------------------
// 12. cn-ram-available
// ---------------------------------------------------------------------------

pub struct CnRamAvailable;
impl Filter for CnRamAvailable {
    fn name(&self) -> &'static str {
        "cn-ram-available"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> Verdict {
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        let overprov = cn
            .placement
            .overprovision_ram
            .unwrap_or(ctx.cluster_overprovision.ram);
        // `overprovision_ram` is the multiplier (matches legacy
        // DAPI semantics): 1.0 = no oversubscription, >1.0 =
        // oversubscribe, <1.0 = conservative safety margin. RAM
        // defaults to 1.0 (no oversubscription -- you can't
        // physically over-commit RAM on this platform).
        let effective_total = ((cap.ram_total_mb as f64) * overprov as f64).floor() as u64;
        let reserved: u64 = cn.active_reservations.iter().map(|r| r.ram_mb).sum();
        let assigned: u64 = cn.assigned_instances.iter().map(|i| i.ram_mb).sum();
        let residual = effective_total
            .saturating_sub(reserved)
            .saturating_sub(assigned);
        if req.ram_mb > residual {
            Verdict::reject(format!(
                "ram needs {} MB, residual {residual} MB \
                 (total={} overprov={overprov:.2} reserved={reserved} assigned={assigned})",
                req.ram_mb, cap.ram_total_mb
            ))
        } else {
            Verdict::Accept
        }
    }
}

// ---------------------------------------------------------------------------
// 13. cn-cpu-available
// ---------------------------------------------------------------------------

pub struct CnCpuAvailable;
impl Filter for CnCpuAvailable {
    fn name(&self) -> &'static str {
        "cn-cpu-available"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, ctx: &ChainContext) -> Verdict {
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        let overprov = cn
            .placement
            .overprovision_cpu
            .unwrap_or(ctx.cluster_overprovision.cpu);
        // 100 cpu_units == 1 vCPU at 1.0 overprovision (legacy
        // DAPI's `cpu_cap` convention). The overprovision ratio
        // is the multiplier (matches legacy DAPI): 4.0 = pack
        // four virtual vCPUs per physical thread; 1.0 = no
        // oversubscription. CPU defaults to 4.0 cluster-wide.
        let raw_total = (cap.cpu_threads_logical as u64) * 100;
        let effective_total = ((raw_total as f64) * overprov as f64).floor() as u64;
        let reserved: u64 = cn
            .active_reservations
            .iter()
            .map(|r| r.cpu_units as u64)
            .sum();
        let assigned: u64 = cn
            .assigned_instances
            .iter()
            .map(|i| i.cpu_units as u64)
            .sum();
        let residual = effective_total
            .saturating_sub(reserved)
            .saturating_sub(assigned);
        if (req.cpu_units as u64) > residual {
            Verdict::reject(format!(
                "cpu needs {} units, residual {residual} units \
                 (threads={} overprov={overprov:.2} reserved={reserved} assigned={assigned})",
                req.cpu_units, cap.cpu_threads_logical
            ))
        } else {
            Verdict::Accept
        }
    }
}

// ---------------------------------------------------------------------------
// 14. cn-numa-fits
//
// Only runs when the request carries a NUMA constraint. v1 does
// not yet model per-instance NUMA pinning on either the request
// side or the reservation side (RFD 00005 README open question);
// PL-3 ships the filter as a clean `Skip` so it's wired into the
// default chain and a future slice can fill in the body without
// touching the chain config.
// ---------------------------------------------------------------------------

pub struct CnNumaFits;
impl Filter for CnNumaFits {
    fn name(&self) -> &'static str {
        "cn-numa-fits"
    }
    fn evaluate(&self, _cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        // No NUMA-pin field on the request yet; always skip.
        Verdict::Skip
    }
}

// ---------------------------------------------------------------------------
// 15. cn-device-available
// ---------------------------------------------------------------------------

pub struct CnDeviceAvailable;
impl Filter for CnDeviceAvailable {
    fn name(&self) -> &'static str {
        "cn-device-available"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if req.required_devices.is_empty() {
            return Verdict::Skip;
        }
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        // Sum per-(kind, model) reservation pressure.
        let mut reserved: std::collections::BTreeMap<(crate::types::DeviceKind, &str), u32> =
            std::collections::BTreeMap::new();
        for r in &cn.active_reservations {
            for d in &r.devices {
                *reserved.entry((d.kind, d.model.as_str())).or_insert(0) += d.count;
            }
        }
        for ask in &req.required_devices {
            let avail: u32 = cap
                .devices
                .iter()
                .filter(|d| d.kind == ask.kind && d.model == ask.model)
                .map(|d| d.free_count)
                .sum();
            let r = reserved
                .get(&(ask.kind, ask.model.as_str()))
                .copied()
                .unwrap_or(0);
            let residual = avail.saturating_sub(r);
            if ask.count > residual {
                return Verdict::reject(format!(
                    "device `{:?}:{}` needs {}, residual {residual} \
                     (free={avail} reserved={r})",
                    ask.kind, ask.model, ask.count
                ));
            }
        }
        Verdict::Accept
    }
}

// ---------------------------------------------------------------------------
// 16. cn-hvm-supported
// ---------------------------------------------------------------------------

pub struct CnHvmSupported;
impl Filter for CnHvmSupported {
    fn name(&self) -> &'static str {
        "cn-hvm-supported"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if !req.needs_hvm {
            return Verdict::Skip;
        }
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        if cap.hvm_supported {
            Verdict::Accept
        } else {
            Verdict::reject("cn does not advertise hardware virtualisation")
        }
    }
}

// ---------------------------------------------------------------------------
// 17. cn-affinity-required
//
// The hard half of the affinity story (`Required` rules);
// `Preferred` rules feed the `score-affinity-preferred` scorer.
//
// v1 supports the two `vm_to_host` selector kinds (`CnUuids`,
// `CnTraitMatch`) end to end. The `vm_to_vm` selectors
// (`InstanceIds`, `InstanceTagMatch`) require sibling-instance
// rules to be visible -- the runner gets that via
// `ChainContext.sibling_instances`, which carries the tenant-
// scoped projection. v1 of the filter checks the `InstanceIds`
// form by consulting `cn.assigned_instances` (the CN's
// host-bound list); the `InstanceTagMatch` form is documented as
// "skip" for now since the per-instance tag map isn't on the
// projection yet -- PL-7 lands tag-level introspection
// alongside the operator surface.
// ---------------------------------------------------------------------------

pub struct CnAffinityRequired;
impl Filter for CnAffinityRequired {
    fn name(&self) -> &'static str {
        "cn-affinity-required"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        let required: Vec<_> = req
            .affinity
            .rules
            .iter()
            .filter(|r| matches!(r.scope, AffinityScope::Required))
            .collect();
        if required.is_empty() && req.affinity.spread.is_none() {
            return Verdict::Skip;
        }
        for rule in &required {
            match (&rule.selector, rule.op) {
                (AffinitySelector::CnUuids(uuids), AffinityOp::In) => {
                    if !uuids.contains(&cn.server_uuid) {
                        return Verdict::reject(format!(
                            "vm-to-host In rule rejected: CN {} not in selector {:?}",
                            cn.server_uuid, uuids
                        ));
                    }
                }
                (AffinitySelector::CnUuids(uuids), AffinityOp::NotIn) => {
                    if uuids.contains(&cn.server_uuid) {
                        return Verdict::reject(format!(
                            "vm-to-host NotIn rule rejected: CN {} appears in selector {:?}",
                            cn.server_uuid, uuids
                        ));
                    }
                }
                (AffinitySelector::CnTraitMatch { key, value }, AffinityOp::In) => {
                    match cn.placement.traits.get(key) {
                        Some(have) if have == value => {}
                        _ => {
                            return Verdict::reject(format!(
                                "vm-to-host trait-In rule rejected: \
                                 CN trait `{key}` is not `{value}`"
                            ));
                        }
                    }
                }
                (AffinitySelector::CnTraitMatch { key, value }, AffinityOp::NotIn) => {
                    if let Some(have) = cn.placement.traits.get(key)
                        && have == value
                    {
                        return Verdict::reject(format!(
                            "vm-to-host trait-NotIn rule rejected: \
                             CN trait `{key}` matches `{value}`"
                        ));
                    }
                }
                (AffinitySelector::InstanceIds(ids), AffinityOp::In) => {
                    let assigned: std::collections::BTreeSet<uuid::Uuid> = cn
                        .assigned_instances
                        .iter()
                        .map(|i| i.instance_id)
                        .collect();
                    if !ids.iter().any(|id| assigned.contains(id)) {
                        return Verdict::reject(format!(
                            "vm-to-vm In rule rejected: none of {ids:?} are on this CN"
                        ));
                    }
                }
                (AffinitySelector::InstanceIds(ids), AffinityOp::NotIn) => {
                    let assigned: std::collections::BTreeSet<uuid::Uuid> = cn
                        .assigned_instances
                        .iter()
                        .map(|i| i.instance_id)
                        .collect();
                    if ids.iter().any(|id| assigned.contains(id)) {
                        return Verdict::reject(format!(
                            "vm-to-vm NotIn rule rejected: one of {ids:?} is on this CN"
                        ));
                    }
                }
                (AffinitySelector::InstanceTagMatch { .. }, _) => {
                    // Tag-based vm-to-vm rules need a per-instance
                    // tag map on the projection that PL-3 does not
                    // yet ship. Skip with a note rather than
                    // pretending we evaluated it.
                    continue;
                }
                // `AffinitySelector` is `#[non_exhaustive]` in
                // `tritond-store`; a future variant lands here
                // until the filter is taught how to evaluate it.
                // Conservative behaviour: skip (treat as not
                // applicable) so a new selector kind doesn't
                // silently start passing every CN.
                _ => continue,
            }
        }
        Verdict::Accept
    }
}

// ---------------------------------------------------------------------------
// 18. cn-not-evacuating
//
// Until `tcadm cn drain` lands (PL-7), this is effectively the
// same check as `cn-not-cordoned` but for `cordoned_reason ==
// Some("drain")`. The filter is wired into the default chain so
// drain-mode CNs surface a distinct reject string in the
// `ExplainReport`.
// ---------------------------------------------------------------------------

pub struct CnNotEvacuating;
impl Filter for CnNotEvacuating {
    fn name(&self) -> &'static str {
        "cn-not-evacuating"
    }
    fn evaluate(&self, cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        match cn.placement.cordoned_reason.as_deref() {
            Some("drain") => Verdict::reject("cn drain in progress (cordoned_reason=drain)"),
            _ => Verdict::Skip,
        }
    }
}

// ---------------------------------------------------------------------------
// Migration filters (LM-0). See `we-need-to-build-ancient-scone.md`
// §2 / §J. These five filters are in the default chain but the four
// source-vs-target comparison filters explicitly `Skip` when
// `req.migration` is `None`, so `instance-create` is unaffected. The
// designate_for_migration saga action populates `req.migration` and
// the filters engage.
// ---------------------------------------------------------------------------

// cn-not-in-avoid-list — reject any CN whose uuid appears in
// `req.avoid_cn`. Migration uses this to stop the chain from picking
// the source CN as its own target. Honored even on the force-place
// path: an operator can't force back onto a CN they explicitly
// avoided.
pub struct CnNotInAvoidList;
impl Filter for CnNotInAvoidList {
    fn name(&self) -> &'static str {
        "cn-not-in-avoid-list"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        if req.avoid_cn.is_empty() {
            return Verdict::Skip;
        }
        if req.avoid_cn.contains(&cn.server_uuid) {
            Verdict::reject(format!(
                "cn {} appears in avoid_cn ({} entries)",
                cn.server_uuid,
                req.avoid_cn.len(),
            ))
        } else {
            Verdict::Accept
        }
    }
}

// cn-bhyve-compatible — target's vmm-migrate wire protocol must
// match the source's. Mismatch means the WebSocket handshake would
// reject and the migration would fail on the wire; we'd rather catch
// it at designate time so the operator sees a clean error.
pub struct CnBhyveCompatible;
impl Filter for CnBhyveCompatible {
    fn name(&self) -> &'static str {
        "cn-bhyve-compatible"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        let Some(mig) = req.migration.as_ref() else {
            return Verdict::Skip;
        };
        let Some(cap) = cn.capacity.as_ref() else {
            // `cn-capacity-present` already rejected; defensive Skip.
            return Verdict::Skip;
        };
        match cap.vmm_protocol_version.as_deref() {
            None => Verdict::reject(
                "cn has not reported vmm_protocol_version (agent capability probe missing)",
            ),
            Some(v) if v == mig.vmm_protocol_version => Verdict::Accept,
            Some(v) => Verdict::reject(format!(
                "vmm protocol mismatch: source={} target={}",
                mig.vmm_protocol_version, v,
            )),
        }
    }
}

// cn-cpu-feature-superset — target CPU feature set must be a
// superset of what bhyve exposes to the guest on the source.
// Missing a feature the guest sees would manifest as a `#UD` (invalid
// opcode) exception on resume, which kernel panics most guests.
pub struct CnCpuFeatureSuperset;
impl Filter for CnCpuFeatureSuperset {
    fn name(&self) -> &'static str {
        "cn-cpu-feature-superset"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        let Some(mig) = req.migration.as_ref() else {
            return Verdict::Skip;
        };
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        if mig.cpu_features.is_empty() {
            // Source reported no features — treat as "no constraint"
            // rather than a wholesale reject. The agent capability
            // probe will fill this in for v1; older agents pass.
            return Verdict::Skip;
        }
        let missing: Vec<&str> = mig
            .cpu_features
            .iter()
            .filter(|f| !cap.cpu_features.iter().any(|c| c == *f))
            .map(|s| s.as_str())
            .collect();
        if missing.is_empty() {
            Verdict::Accept
        } else {
            Verdict::reject(format!(
                "target missing cpu features: {}",
                missing.join(", "),
            ))
        }
    }
}

// cn-time-synced — target's clock must be within 100 ms of the
// source's. bhyve imports TSC + wall-clock state during the
// device-state handoff; a large skew between source and target
// surfaces as a guest-visible clock jump on resume which some
// userspaces (notably databases with leases / fencing) treat as a
// fatal error.
pub struct CnTimeSynced {
    /// Maximum permitted target-vs-source clock offset, absolute
    /// value in nanoseconds. Defaults to 100ms; the configuration
    /// loader can tune this via cluster settings later.
    pub max_skew_ns: i64,
}

impl Default for CnTimeSynced {
    fn default() -> Self {
        Self {
            max_skew_ns: 100_000_000, // 100ms
        }
    }
}

impl Filter for CnTimeSynced {
    fn name(&self) -> &'static str {
        "cn-time-synced"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        let Some(mig) = req.migration.as_ref() else {
            return Verdict::Skip;
        };
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        let Some(target_offset) = cap.tsc_offset_ns else {
            return Verdict::reject(
                "cn has not reported tsc_offset_ns (agent capability probe missing)",
            );
        };
        let skew_ns = mig.tsc_offset_ns.saturating_sub(target_offset).abs();
        if skew_ns > self.max_skew_ns {
            Verdict::reject(format!(
                "clock skew {}ns exceeds threshold {}ns",
                skew_ns, self.max_skew_ns,
            ))
        } else {
            Verdict::Accept
        }
    }
}

// cn-zfs-compatible — for every source pool, the target must have a
// pool with matching on-disk-format properties (encryption,
// compression, recordsize). Mismatch means `zfs recv` would fail or
// the resulting dataset would have different semantics than the
// source.
pub struct CnZfsCompatible;
impl Filter for CnZfsCompatible {
    fn name(&self) -> &'static str {
        "cn-zfs-compatible"
    }
    fn evaluate(&self, cn: &CnView, req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        let Some(mig) = req.migration.as_ref() else {
            return Verdict::Skip;
        };
        let Some(cap) = cn.capacity.as_ref() else {
            return Verdict::Skip;
        };
        if mig.zpool_props.is_empty() {
            // No source pools to compare; this is a misuse, not a
            // CN problem.
            return Verdict::Skip;
        }
        for (pool, src) in &mig.zpool_props {
            let Some(dst) = cap.zpool_props.get(pool) else {
                return Verdict::reject(format!("target missing zpool {pool} (source uses it)"));
            };
            if src.encryption != dst.encryption {
                return Verdict::reject(format!(
                    "zpool {pool} encryption mismatch: source={} target={}",
                    src.encryption, dst.encryption,
                ));
            }
            if src.compression != dst.compression {
                return Verdict::reject(format!(
                    "zpool {pool} compression mismatch: source={} target={}",
                    src.compression, dst.compression,
                ));
            }
            if src.recordsize_bytes != dst.recordsize_bytes {
                return Verdict::reject(format!(
                    "zpool {pool} recordsize mismatch: source={} target={}",
                    src.recordsize_bytes, dst.recordsize_bytes,
                ));
            }
        }
        Verdict::Accept
    }
}

// ---------------------------------------------------------------------------
// Opt-in guardrail: cn-load-not-overheating (D-Pl-9)
// ---------------------------------------------------------------------------

pub struct CnLoadNotOverheating {
    /// Threshold the operator opted in to (default 0.90 per the
    /// RFD if a default is requested by the cluster-settings
    /// loader). Filters carry their own configuration; the
    /// default chain does not include this filter at all.
    pub cpu_p95_5m_max: f32,
}

impl Default for CnLoadNotOverheating {
    fn default() -> Self {
        Self {
            cpu_p95_5m_max: 0.90,
        }
    }
}

impl Filter for CnLoadNotOverheating {
    fn name(&self) -> &'static str {
        "cn-load-not-overheating"
    }
    fn evaluate(&self, cn: &CnView, _req: &PlacementRequest, _ctx: &ChainContext) -> Verdict {
        let Some(load) = cn.load_summary.as_ref() else {
            return Verdict::Skip;
        };
        if load.stale {
            return Verdict::Skip;
        }
        if load.cpu_p95_5m > self.cpu_p95_5m_max {
            Verdict::reject(format!(
                "cpu_p95_5m {:.2} > guardrail {:.2}",
                load.cpu_p95_5m, self.cpu_p95_5m_max
            ))
        } else {
            Verdict::Accept
        }
    }
}

// ---------------------------------------------------------------------------
// Default chain.
// ---------------------------------------------------------------------------

/// Default registration order for the built-in filters, matching
/// RFD 00005 doc 02 §"The seventeen built-in filters" plus the five
/// LM-0 migration filters. `cn-load-not-overheating` is *not*
/// included -- operators opt in via cluster settings (D-Pl-9).
///
/// The migration filters (`cn-not-in-avoid-list`,
/// `cn-bhyve-compatible`, `cn-cpu-feature-superset`,
/// `cn-time-synced`, `cn-zfs-compatible`) are included by default
/// but `Verdict::Skip` when the request doesn't carry the
/// migration-shape inputs (`avoid_cn` / `migration`). `instance-create`
/// requests are unaffected; only the migration designate action
/// engages them.
pub fn default_filter_chain() -> Vec<Arc<dyn Filter>> {
    vec![
        Arc::new(CnApprovedAndLive),
        // Migration: avoid_cn is honored regardless of feature data
        // availability, so it goes right after the liveness gate so
        // a force-place against a CN that's been explicitly avoided
        // gets rejected as cheaply as possible.
        Arc::new(CnNotInAvoidList),
        Arc::new(CnCapacityPresent),
        Arc::new(CnRoleMatches),
        Arc::new(CnNotReserved),
        Arc::new(CnNotCordoned),
        Arc::new(CnNotEvacuating),
        Arc::new(CnScopeMatch),
        Arc::new(CnPlatformMin),
        Arc::new(CnTraitsRequired),
        Arc::new(CnNicTags),
        Arc::new(CnUnderlay),
        Arc::new(CnZpoolHasSpace),
        Arc::new(CnRamAvailable),
        Arc::new(CnCpuAvailable),
        Arc::new(CnNumaFits),
        Arc::new(CnDeviceAvailable),
        Arc::new(CnHvmSupported),
        Arc::new(CnAffinityRequired),
        // Migration compat checks. All four `Skip` when
        // `req.migration` is `None`, so the chain is unchanged for
        // `instance-create`. They run only when the designate
        // saga action populates the source fingerprint.
        Arc::new(CnBhyveCompatible),
        Arc::new(CnCpuFeatureSuperset),
        Arc::new(CnTimeSynced::default()),
        Arc::new(CnZfsCompatible),
    ]
}

// ---------------------------------------------------------------------------
// Helper for the placement-policy view (a missing field on
// `PlacementPolicyView` -- the projection doesn't carry
// `overprovision_disk` because PL-1 didn't anticipate it. Add a
// thin accessor here so the zpool filter compiles; a follow-up
// either adds the field to the projection or moves the lookup
// into `ChainContext`. PL-3 keeps the smallest diff possible.)
// ---------------------------------------------------------------------------

impl crate::types::PlacementPolicyView {
    /// Per-CN disk overprovision override. The projection doesn't
    /// model this field yet; PL-3 returns `None` so callers fall
    /// back to the cluster default. Filling it in is a PL-7
    /// follow-up paired with the operator surface.
    pub fn overprovision_disk(&self) -> Option<f32> {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OverprovisionDefaults;
    use crate::types::{
        AssignedInstanceView, CapacityView, CnLoadSummaryView, CnRoleView, CnStateView, CnView,
        DeviceKind as PlacementDeviceKind, DeviceView, NumaNodeView, PlacementPolicyView,
        PlacementRequest, ReservationView, SiblingInstanceView, StorageTier, StrategyWeights,
        UnderlayCapability, ZpoolView,
    };
    use chrono::{Duration, Utc};
    use std::collections::BTreeMap;
    use tritond_store::InstanceAffinity;
    use uuid::Uuid;

    fn nil() -> Uuid {
        Uuid::nil()
    }

    fn now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    fn make_cn() -> CnView {
        CnView {
            server_uuid: Uuid::new_v4(),
            hostname: "cn-x".into(),
            state: CnStateView::Approved,
            role: CnRoleView::Tenant,
            last_seen: Some(now()),
            capacity: Some(CapacityView {
                cpu_cores_physical: 16,
                cpu_threads_logical: 32,
                numa_nodes: vec![NumaNodeView {
                    node_id: 0,
                    cores: 16,
                    ram_mb: 65_536,
                }],
                ram_total_mb: 65_536,
                ram_available_mb: 60_000,
                cpu_utilization_pct: 0.10,
                zpools: vec![ZpoolView {
                    name: "zones".into(),
                    total_bytes: 1_000_000_000_000,
                    free_bytes: 800_000_000_000,
                    tier: StorageTier::Ssd,
                }],
                nic_tags: vec!["admin".into(), "external".into()],
                underlay: UnderlayCapability {
                    ipv4: true,
                    ipv6: false,
                },
                devices: Vec::new(),
                platform_version: "20260501T000000Z".into(),
                reported_at: now(),
                hvm_supported: true,
                vmm_protocol_version: None,
                cpu_features: Vec::new(),
                tsc_offset_ns: None,
                zpool_props: BTreeMap::new(),
            }),
            placement: PlacementPolicyView::default(),
            active_reservations: Vec::new(),
            load_summary: None,
            assigned_instances: Vec::new(),
        }
    }

    fn make_req() -> PlacementRequest {
        let id = Uuid::new_v4();
        PlacementRequest {
            instance_id: id,
            silo_uuid: nil(),
            tenant_uuid: nil(),
            project_uuid: nil(),
            role: CnRoleView::Tenant,
            cpu_units: 100,
            ram_mb: 2_048,
            disk: BTreeMap::new(),
            required_traits: BTreeMap::new(),
            required_nic_tags: Vec::new(),
            required_underlay: UnderlayCapability {
                ipv4: true,
                ipv6: false,
            },
            required_devices: Vec::new(),
            needs_hvm: false,
            min_platform: None,
            affinity: InstanceAffinity::empty(id, nil(), now()),
            strategy_override: None,
            force_cn: None,
            ignore_scope_pin: false,
            deadline: now() + Duration::minutes(5),
            avoid_cn: Vec::new(),
            migration: None,
        }
    }

    fn make_ctx<'a>(weights: &'a StrategyWeights) -> ChainContext<'a> {
        ChainContext {
            now: now(),
            cluster_overprovision: OverprovisionDefaults::default(),
            load_staleness_secs: 180,
            agent_heartbeat_threshold_secs: 60,
            strategy_weights: weights,
            sibling_instances: &[],
        }
    }

    // ---- cn-approved-and-live ----

    #[test]
    fn cn_approved_and_live_accepts_fresh_approved() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnApprovedAndLive.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    #[test]
    fn cn_approved_and_live_rejects_pending() {
        let mut cn = make_cn();
        cn.state = CnStateView::Pending;
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnApprovedAndLive.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_approved_and_live_rejects_stale_heartbeat() {
        let mut cn = make_cn();
        cn.last_seen = Some(now() - Duration::seconds(600));
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnApprovedAndLive.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_approved_and_live_rejects_never_seen() {
        let mut cn = make_cn();
        cn.last_seen = None;
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnApprovedAndLive.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    // ---- cn-capacity-present ----

    #[test]
    fn cn_capacity_present_accepts_when_capacity_is_some() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnCapacityPresent.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    #[test]
    fn cn_capacity_present_rejects_when_capacity_is_none() {
        let mut cn = make_cn();
        cn.capacity = None;
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnCapacityPresent.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    // ---- cn-role-matches ----

    #[test]
    fn cn_role_matches_tenant_accepts_tenant_request() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnRoleMatches.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    #[test]
    fn cn_role_matches_edge_rejects_tenant_request() {
        let mut cn = make_cn();
        cn.role = CnRoleView::Edge;
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnRoleMatches.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_role_matches_both_accepts_either() {
        let mut cn = make_cn();
        cn.role = CnRoleView::Both;
        let mut req = make_req();
        req.role = CnRoleView::Tenant;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnRoleMatches.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
        req.role = CnRoleView::Edge;
        assert!(matches!(
            CnRoleMatches.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-not-reserved ----

    #[test]
    fn cn_not_reserved_accepts_when_not_reserved() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNotReserved.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    #[test]
    fn cn_not_reserved_rejects_when_reserved_without_matching_force() {
        let mut cn = make_cn();
        cn.placement.reserved = true;
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNotReserved.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_not_reserved_accepts_when_reserved_but_force_targets_this_cn() {
        let mut cn = make_cn();
        cn.placement.reserved = true;
        let mut req = make_req();
        req.force_cn = Some(cn.server_uuid);
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNotReserved.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-not-cordoned ----

    #[test]
    fn cn_not_cordoned_rejects_cordoned() {
        let mut cn = make_cn();
        cn.placement.cordoned = true;
        cn.placement.cordoned_reason = Some("maintenance".into());
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        match CnNotCordoned.evaluate(&cn, &req, &ctx) {
            Verdict::Reject { reason } => assert!(reason.contains("cordoned")),
            v => panic!("expected reject, got {v:?}"),
        }
    }

    // ---- cn-scope-match ----

    #[test]
    fn cn_scope_match_rejects_silo_pin_mismatch() {
        let mut cn = make_cn();
        cn.placement.pinned_silo_uuid = Some(Uuid::new_v4());
        let req = make_req(); // silo_uuid == nil()
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnScopeMatch.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_scope_match_rejects_tenant_pin_mismatch() {
        let mut cn = make_cn();
        cn.placement.pinned_tenant_uuid = Some(Uuid::new_v4());
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnScopeMatch.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_scope_match_accepts_matching_pins() {
        let mut cn = make_cn();
        let silo = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        cn.placement.pinned_silo_uuid = Some(silo);
        cn.placement.pinned_tenant_uuid = Some(tenant);
        let mut req = make_req();
        req.silo_uuid = silo;
        req.tenant_uuid = tenant;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnScopeMatch.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    #[test]
    fn cn_scope_match_bypass_requires_force_targeting_this_cn() {
        let mut cn = make_cn();
        cn.placement.pinned_silo_uuid = Some(Uuid::new_v4());
        let mut req = make_req();
        req.ignore_scope_pin = true;
        // Force targets a different CN; bypass does not engage.
        req.force_cn = Some(Uuid::new_v4());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnScopeMatch.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
        // Now point the force at this CN -- bypass engages.
        req.force_cn = Some(cn.server_uuid);
        assert!(matches!(
            CnScopeMatch.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-platform-min ----

    #[test]
    fn cn_platform_min_skips_when_no_constraint() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnPlatformMin.evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    #[test]
    fn cn_platform_min_rejects_older_platform() {
        let cn = make_cn();
        let mut req = make_req();
        req.min_platform = Some("20990101T000000Z".into());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnPlatformMin.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_platform_min_accepts_newer_or_equal() {
        let cn = make_cn();
        let mut req = make_req();
        req.min_platform = Some("20200101T000000Z".into());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnPlatformMin.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-traits-required ----

    #[test]
    fn cn_traits_required_rejects_missing_trait() {
        let cn = make_cn();
        let mut req = make_req();
        req.required_traits.insert("gpu".into(), "a100".into());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnTraitsRequired.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_traits_required_rejects_different_value() {
        let mut cn = make_cn();
        cn.placement.traits.insert("gpu".into(), "v100".into());
        let mut req = make_req();
        req.required_traits.insert("gpu".into(), "a100".into());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnTraitsRequired.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_traits_required_accepts_matching() {
        let mut cn = make_cn();
        cn.placement.traits.insert("gpu".into(), "a100".into());
        let mut req = make_req();
        req.required_traits.insert("gpu".into(), "a100".into());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnTraitsRequired.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-nic-tags ----

    #[test]
    fn cn_nic_tags_rejects_missing_tag() {
        let cn = make_cn();
        let mut req = make_req();
        req.required_nic_tags.push("private-overlay".into());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNicTags.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_nic_tags_accepts_present_tag() {
        let cn = make_cn();
        let mut req = make_req();
        req.required_nic_tags.push("admin".into());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNicTags.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-underlay ----

    #[test]
    fn cn_underlay_rejects_when_v6_required_but_only_v4() {
        let cn = make_cn(); // capacity.underlay = { ipv4: true, ipv6: false }
        let mut req = make_req();
        req.required_underlay.ipv6 = true;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnUnderlay.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_underlay_accepts_when_capability_covers_request() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().underlay.ipv6 = true;
        let mut req = make_req();
        req.required_underlay.ipv6 = true;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnUnderlay.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-zpool-has-space ----

    #[test]
    fn cn_zpool_rejects_when_pool_missing() {
        let cn = make_cn();
        let mut req = make_req();
        req.disk.insert("data-tank".into(), 10_000_000);
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnZpoolHasSpace.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_zpool_rejects_when_ask_exceeds_residual() {
        let cn = make_cn();
        let mut req = make_req();
        req.disk.insert("zones".into(), 10_000_000_000_000); // ask > free
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnZpoolHasSpace.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_zpool_accepts_when_fits() {
        let cn = make_cn();
        let mut req = make_req();
        req.disk.insert("zones".into(), 10_000_000); // small ask
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnZpoolHasSpace.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    #[test]
    fn cn_zpool_subtracts_active_reservations() {
        let mut cn = make_cn();
        cn.active_reservations.push(ReservationView {
            saga_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            cpu_units: 0,
            ram_mb: 0,
            disk: {
                let mut m = BTreeMap::new();
                m.insert("zones".into(), 790_000_000_000);
                m
            },
            devices: Vec::new(),
            deadline: now(),
        });
        // capacity free was 800 GB; after subtracting 790 GB
        // reservation the residual is ~10 GB. A 20 GB ask must
        // reject.
        let mut req = make_req();
        req.disk.insert("zones".into(), 20_000_000_000);
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnZpoolHasSpace.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    // ---- cn-ram-available ----

    #[test]
    fn cn_ram_available_rejects_oversized_ask() {
        let cn = make_cn(); // ram_total_mb = 65536
        let mut req = make_req();
        req.ram_mb = 100_000;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnRamAvailable.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_ram_available_subtracts_reservations_and_assigned() {
        let mut cn = make_cn();
        cn.active_reservations.push(ReservationView {
            saga_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            cpu_units: 0,
            ram_mb: 30_000,
            disk: BTreeMap::new(),
            devices: Vec::new(),
            deadline: now(),
        });
        cn.assigned_instances.push(AssignedInstanceView {
            instance_id: Uuid::new_v4(),
            silo_uuid: nil(),
            tenant_uuid: nil(),
            cpu_units: 0,
            ram_mb: 30_000,
        });
        // 65536 - 30000 - 30000 = 5536 residual; 6000 ask must reject.
        let mut req = make_req();
        req.ram_mb = 6_000;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnRamAvailable.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    // ---- cn-cpu-available ----

    #[test]
    fn cn_cpu_available_rejects_oversized_ask() {
        // cpu_threads_logical = 32; cluster default cpu overprov =
        // 4.0 → effective = 32 * 100 * 4.0 = 12800 cpu_units. An
        // ask above that must reject.
        let cn = make_cn();
        let mut req = make_req();
        req.cpu_units = 20_000;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnCpuAvailable.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_cpu_available_accepts_when_fits_under_default_overprov() {
        // 32 threads × 100 × 4.0 overprov = 12800 cpu_units → a
        // small ask fits comfortably.
        let cn = make_cn();
        let req = make_req(); // cpu_units = 100
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnCpuAvailable.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-device-available ----

    #[test]
    fn cn_device_rejects_when_model_absent() {
        let cn = make_cn(); // no devices
        let mut req = make_req();
        req.required_devices.push(crate::types::DeviceReservation {
            kind: PlacementDeviceKind::Gpu,
            model: "a100".into(),
            count: 1,
        });
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnDeviceAvailable.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_device_accepts_when_residual_fits() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().devices.push(DeviceView {
            kind: PlacementDeviceKind::Gpu,
            model: "a100".into(),
            free_count: 4,
        });
        let mut req = make_req();
        req.required_devices.push(crate::types::DeviceReservation {
            kind: PlacementDeviceKind::Gpu,
            model: "a100".into(),
            count: 2,
        });
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnDeviceAvailable.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    #[test]
    fn cn_device_subtracts_active_reservation() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().devices.push(DeviceView {
            kind: PlacementDeviceKind::Gpu,
            model: "a100".into(),
            free_count: 4,
        });
        cn.active_reservations.push(ReservationView {
            saga_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            cpu_units: 0,
            ram_mb: 0,
            disk: BTreeMap::new(),
            devices: vec![crate::types::DeviceReservation {
                kind: PlacementDeviceKind::Gpu,
                model: "a100".into(),
                count: 3,
            }],
            deadline: now(),
        });
        let mut req = make_req();
        req.required_devices.push(crate::types::DeviceReservation {
            kind: PlacementDeviceKind::Gpu,
            model: "a100".into(),
            count: 2,
        });
        // free=4 reserved=3 → residual=1 < ask=2
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnDeviceAvailable.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    // ---- cn-hvm-supported ----

    #[test]
    fn cn_hvm_skips_when_request_does_not_need_hvm() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().hvm_supported = false;
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnHvmSupported.evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    #[test]
    fn cn_hvm_rejects_when_required_but_unsupported() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().hvm_supported = false;
        let mut req = make_req();
        req.needs_hvm = true;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnHvmSupported.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_hvm_accepts_when_required_and_supported() {
        let cn = make_cn(); // hvm_supported = true
        let mut req = make_req();
        req.needs_hvm = true;
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnHvmSupported.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-affinity-required ----

    #[test]
    fn cn_affinity_skips_when_no_rules() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnAffinityRequired.evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    #[test]
    fn cn_affinity_rejects_required_cn_uuids_when_this_cn_not_in_set() {
        use tritond_store::{AffinityKind, AffinityRule, AffinitySelector};
        let cn = make_cn();
        let mut req = make_req();
        req.affinity.rules.push(AffinityRule {
            kind: AffinityKind::VmToHost,
            scope: tritond_store::AffinityScope::Required,
            op: tritond_store::AffinityOp::In,
            selector: AffinitySelector::CnUuids(vec![Uuid::new_v4()]),
        });
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnAffinityRequired.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_affinity_rejects_required_cn_uuids_notin_when_this_cn_is_in_set() {
        use tritond_store::{AffinityKind, AffinityRule, AffinitySelector};
        let cn = make_cn();
        let mut req = make_req();
        req.affinity.rules.push(AffinityRule {
            kind: AffinityKind::VmToHost,
            scope: tritond_store::AffinityScope::Required,
            op: tritond_store::AffinityOp::NotIn,
            selector: AffinitySelector::CnUuids(vec![cn.server_uuid]),
        });
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnAffinityRequired.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_affinity_accepts_when_required_cn_trait_match_holds() {
        use tritond_store::{AffinityKind, AffinityRule, AffinitySelector};
        let mut cn = make_cn();
        cn.placement.traits.insert("rack".into(), "r3".into());
        let mut req = make_req();
        req.affinity.rules.push(AffinityRule {
            kind: AffinityKind::VmToHost,
            scope: tritond_store::AffinityScope::Required,
            op: tritond_store::AffinityOp::In,
            selector: AffinitySelector::CnTraitMatch {
                key: "rack".into(),
                value: "r3".into(),
            },
        });
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnAffinityRequired.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-not-evacuating ----

    #[test]
    fn cn_not_evacuating_rejects_when_cordoned_reason_is_drain() {
        let mut cn = make_cn();
        cn.placement.cordoned = true;
        cn.placement.cordoned_reason = Some("drain".into());
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNotEvacuating.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_not_evacuating_skips_when_not_draining() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNotEvacuating.evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    // ---- cn-load-not-overheating (opt-in) ----

    #[test]
    fn cn_load_not_overheating_skips_on_stale_or_missing_summary() {
        let cn = make_cn(); // load_summary = None
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        let filter = CnLoadNotOverheating::default();
        assert!(matches!(filter.evaluate(&cn, &req, &ctx), Verdict::Skip));
    }

    #[test]
    fn cn_load_not_overheating_rejects_above_threshold() {
        let mut cn = make_cn();
        cn.load_summary = Some(CnLoadSummaryView {
            last_refreshed_at: now(),
            stale: false,
            cpu_p50_5m: 0.5,
            cpu_p95_5m: 0.95,
            cpu_p50_1d: 0.3,
            cpu_p95_1d: 0.4,
            cpu_p95_7d: 0.5,
            ram_used_p95_5m: 0.5,
            nic_tx_bps_p95_5m: 0,
            nic_rx_bps_p95_5m: 0,
        });
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        let filter = CnLoadNotOverheating::default(); // threshold 0.90
        assert!(matches!(
            filter.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    // ---- default chain integration ----

    #[test]
    fn default_chain_accepts_fresh_cn_with_default_request() {
        // A fresh CN with a default request and no operator
        // policy should pass every filter in the default chain.
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        for filter in default_filter_chain() {
            let v = filter.evaluate(&cn, &req, &ctx);
            assert!(
                !matches!(v, Verdict::Reject { .. }),
                "filter {} rejected unexpectedly: {v:?}",
                filter.name()
            );
        }
    }

    #[test]
    fn default_chain_size_is_twenty_three() {
        // 17 RFD-00005 filters + `cn-capacity-present` (the explicit
        // "row absent" check factored out of the per-filter
        // `if let Some(cap)` boilerplate) + 5 LM-0 migration filters
        // (`cn-not-in-avoid-list`, `cn-bhyve-compatible`,
        // `cn-cpu-feature-superset`, `cn-time-synced`,
        // `cn-zfs-compatible`). The opt-in `cn-load-not-overheating`
        // is NOT in the default.
        assert_eq!(default_filter_chain().len(), 23);
    }

    // ---- cn-not-in-avoid-list (LM-0) ----

    #[test]
    fn cn_not_in_avoid_list_skips_when_avoid_empty() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNotInAvoidList.evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    #[test]
    fn cn_not_in_avoid_list_rejects_when_this_cn_is_avoided() {
        let cn = make_cn();
        let mut req = make_req();
        req.avoid_cn.push(cn.server_uuid);
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        match CnNotInAvoidList.evaluate(&cn, &req, &ctx) {
            Verdict::Reject { reason } => assert!(reason.contains("avoid_cn")),
            v => panic!("expected reject, got {v:?}"),
        }
    }

    #[test]
    fn cn_not_in_avoid_list_rejects_even_under_force_place() {
        // Force-place must NOT override the avoid list: an operator
        // asking the chain to land on a CN they also told it to
        // avoid is a contradiction; we reject the placement rather
        // than honor the force.
        let cn = make_cn();
        let mut req = make_req();
        req.avoid_cn.push(cn.server_uuid);
        req.force_cn = Some(cn.server_uuid);
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNotInAvoidList.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_not_in_avoid_list_accepts_when_different_cn_avoided() {
        let cn = make_cn();
        let mut req = make_req();
        req.avoid_cn.push(Uuid::new_v4()); // some other CN
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnNotInAvoidList.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-bhyve-compatible (LM-0) ----

    fn migration_compat_v0() -> crate::types::MigrationCompat {
        crate::types::MigrationCompat {
            vmm_protocol_version: "vmm-migrate-ron/0".into(),
            cpu_features: vec!["vmx".into(), "avx2".into()],
            tsc_offset_ns: 0,
            zpool_props: BTreeMap::new(),
            source_dataset_encrypted: false,
        }
    }

    #[test]
    fn cn_bhyve_compatible_skips_without_migration_context() {
        let cn = make_cn();
        let req = make_req(); // migration: None
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnBhyveCompatible.evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    #[test]
    fn cn_bhyve_compatible_rejects_when_target_missing_version() {
        let cn = make_cn(); // vmm_protocol_version: None
        let mut req = make_req();
        req.migration = Some(migration_compat_v0());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnBhyveCompatible.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_bhyve_compatible_rejects_on_version_mismatch() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().vmm_protocol_version = Some("vmm-migrate-ron/1".into());
        let mut req = make_req();
        req.migration = Some(migration_compat_v0());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnBhyveCompatible.evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_bhyve_compatible_accepts_on_version_match() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().vmm_protocol_version = Some("vmm-migrate-ron/0".into());
        let mut req = make_req();
        req.migration = Some(migration_compat_v0());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnBhyveCompatible.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-cpu-feature-superset (LM-0) ----

    #[test]
    fn cn_cpu_feature_superset_skips_without_migration_context() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnCpuFeatureSuperset.evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    #[test]
    fn cn_cpu_feature_superset_rejects_when_target_missing_feature() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().cpu_features = vec!["vmx".into()]; // missing avx2
        let mut req = make_req();
        req.migration = Some(migration_compat_v0()); // wants vmx + avx2
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        match CnCpuFeatureSuperset.evaluate(&cn, &req, &ctx) {
            Verdict::Reject { reason } => assert!(reason.contains("avx2")),
            v => panic!("expected reject, got {v:?}"),
        }
    }

    #[test]
    fn cn_cpu_feature_superset_accepts_when_target_is_superset() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().cpu_features =
            vec!["vmx".into(), "avx2".into(), "aes".into()]; // superset
        let mut req = make_req();
        req.migration = Some(migration_compat_v0());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnCpuFeatureSuperset.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-time-synced (LM-0) ----

    #[test]
    fn cn_time_synced_skips_without_migration_context() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnTimeSynced::default().evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    #[test]
    fn cn_time_synced_rejects_when_target_offset_unknown() {
        let cn = make_cn(); // tsc_offset_ns: None
        let mut req = make_req();
        req.migration = Some(migration_compat_v0());
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnTimeSynced::default().evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_time_synced_rejects_above_threshold() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().tsc_offset_ns = Some(500_000_000); // 500ms
        let mut req = make_req();
        req.migration = Some(migration_compat_v0()); // source offset 0
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnTimeSynced::default().evaluate(&cn, &req, &ctx),
            Verdict::Reject { .. }
        ));
    }

    #[test]
    fn cn_time_synced_accepts_within_threshold() {
        let mut cn = make_cn();
        cn.capacity.as_mut().unwrap().tsc_offset_ns = Some(50_000_000); // 50ms
        let mut req = make_req();
        req.migration = Some(migration_compat_v0()); // source offset 0
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnTimeSynced::default().evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    // ---- cn-zfs-compatible (LM-0) ----

    fn zones_pool() -> crate::types::ZpoolPropFingerprint {
        crate::types::ZpoolPropFingerprint {
            encryption: "off".into(),
            compression: "lz4".into(),
            recordsize_bytes: 131_072,
        }
    }

    #[test]
    fn cn_zfs_compatible_skips_without_migration_context() {
        let cn = make_cn();
        let req = make_req();
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnZfsCompatible.evaluate(&cn, &req, &ctx),
            Verdict::Skip
        ));
    }

    #[test]
    fn cn_zfs_compatible_rejects_when_target_missing_pool() {
        let cn = make_cn(); // empty zpool_props
        let mut mig = migration_compat_v0();
        mig.zpool_props.insert("zones".into(), zones_pool());
        let mut req = make_req();
        req.migration = Some(mig);
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        match CnZfsCompatible.evaluate(&cn, &req, &ctx) {
            Verdict::Reject { reason } => assert!(reason.contains("zones")),
            v => panic!("expected reject, got {v:?}"),
        }
    }

    #[test]
    fn cn_zfs_compatible_rejects_on_compression_mismatch() {
        let mut cn = make_cn();
        let mut target_pool = zones_pool();
        target_pool.compression = "zstd".into();
        cn.capacity
            .as_mut()
            .unwrap()
            .zpool_props
            .insert("zones".into(), target_pool);
        let mut mig = migration_compat_v0();
        mig.zpool_props.insert("zones".into(), zones_pool());
        let mut req = make_req();
        req.migration = Some(mig);
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        match CnZfsCompatible.evaluate(&cn, &req, &ctx) {
            Verdict::Reject { reason } => assert!(reason.contains("compression")),
            v => panic!("expected reject, got {v:?}"),
        }
    }

    #[test]
    fn cn_zfs_compatible_accepts_on_match() {
        let mut cn = make_cn();
        cn.capacity
            .as_mut()
            .unwrap()
            .zpool_props
            .insert("zones".into(), zones_pool());
        let mut mig = migration_compat_v0();
        mig.zpool_props.insert("zones".into(), zones_pool());
        let mut req = make_req();
        req.migration = Some(mig);
        let weights = StrategyWeights::new();
        let ctx = make_ctx(&weights);
        assert!(matches!(
            CnZfsCompatible.evaluate(&cn, &req, &ctx),
            Verdict::Accept
        ));
    }

    #[test]
    fn sibling_instances_field_is_accessible_to_filters() {
        // Documents that ctx.sibling_instances is in scope. PL-4
        // scorers will read it; PL-3 filters don't, but the field
        // must compile against the borrow.
        let one = SiblingInstanceView {
            instance_id: Uuid::new_v4(),
            silo_uuid: nil(),
            tenant_uuid: nil(),
            project_uuid: nil(),
            host_cn_uuid: None,
            host_fault_domain: None,
        };
        let weights = StrategyWeights::new();
        let mut ctx = make_ctx(&weights);
        let slice = vec![one];
        ctx.sibling_instances = &slice;
        // Just a compile-time check; no assertion needed.
        assert_eq!(ctx.sibling_instances.len(), 1);
    }
}
