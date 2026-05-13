// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-CN IMDS binding table -- the reverse lookup the IMDS HTTP
//! listener uses to recover caller identity from the connection's
//! peer address.
//!
//! ## Why this exists
//!
//! `IMDS_DESIGN.md` §2.1 / §3 / §6: the proteus kmod redirects a
//! guest's IMDS-bound traffic via `RouteTarget::LocalImds`, SNAT'ing
//! the guest source to a CN-unique pseudo-address recorded in the
//! port blueprint as `ImdsBinding { pseudo_src, instance_id }`. The
//! tritonagent IMDS daemon (see `crate::imds`) `accept()`s on the
//! redirect's destination socket, reads the peer address, and looks
//! it up here to derive `(port_id, instance_id)` -- the design's
//! "Nitro card" caller-identity model. **Identity is never anything
//! the guest sends.**
//!
//! This module owns just the table; populating it from each port
//! blueprint's `imds` field is the proteus apply path's job (a
//! follow-up commit hooks `proteus::apply_blueprint`).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};

use uuid::Uuid;

/// One `(port_id, instance_id)` pair recovered from a peer address.
/// Cheap to clone; the listener passes it down the request stack
/// instead of re-looking up on every claim check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedBinding {
    pub port_id: Uuid,
    pub instance_id: Uuid,
}

/// Per-CN binding table. Cheaply cloneable -- wraps an
/// `Arc<RwLock<HashMap<_, _>>>` so the IMDS listener task and the
/// proteus apply path can share a snapshot without contention on
/// the read path (which is the IMDS hot path).
#[derive(Clone, Default)]
pub struct ImdsBindingTable {
    inner: Arc<RwLock<HashMap<IpAddr, ResolvedBinding>>>,
}

impl ImdsBindingTable {
    /// New empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve `(port_id, instance_id)` from a connection's peer
    /// address. `None` for any peer the proteus apply path hasn't
    /// registered -- the listener returns 403 in that case (the
    /// design's "unknown peer" rule, §3).
    ///
    /// `RwLock` poison is treated as a soft failure: the inner data
    /// is consistent (we never write while panicking), so we recover
    /// and return whatever the table currently holds. Crashing the
    /// IMDS hot path on a panic in some unrelated tritonagent thread
    /// would be worse.
    #[must_use]
    pub fn lookup(&self, peer: IpAddr) -> Option<ResolvedBinding> {
        match self.inner.read() {
            Ok(g) => g.get(&peer).copied(),
            Err(poisoned) => poisoned.into_inner().get(&peer).copied(),
        }
    }

    /// Register `(pseudo_src -> port_id, instance_id)`. Idempotent;
    /// a second insert overwrites (e.g. if proteus re-applies a
    /// blueprint with the same pseudo-source). Returns the prior
    /// resolution, if any, so the caller can spot a remapping.
    pub fn insert(
        &self,
        pseudo_src: IpAddr,
        port_id: Uuid,
        instance_id: Uuid,
    ) -> Option<ResolvedBinding> {
        let entry = ResolvedBinding {
            port_id,
            instance_id,
        };
        let mut g = match self.inner.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.insert(pseudo_src, entry)
    }

    /// Remove every binding whose `port_id` matches. Used on port
    /// delete; we'd otherwise leak an entry per gone-away port.
    ///
    /// Returns the count removed (debug-only -- the agent doesn't
    /// branch on it).
    pub fn remove_by_port(&self, port_id: Uuid) -> usize {
        let mut g = match self.inner.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let before = g.len();
        g.retain(|_, b| b.port_id != port_id);
        before - g.len()
    }

    /// Drop a single pseudo-source mapping (for the case where the
    /// proteus apply path tells us "this port's SNAT pseudo-source
    /// changed" without taking the port down).
    pub fn remove(&self, pseudo_src: IpAddr) -> Option<ResolvedBinding> {
        let mut g = match self.inner.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.remove(&pseudo_src)
    }

    /// Current table size. Diagnostics + tests only.
    pub fn len(&self) -> usize {
        match self.inner.read() {
            Ok(g) => g.len(),
            Err(p) => p.into_inner().len(),
        }
    }

    /// Whether the table is empty. Diagnostics + tests only.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn uuids() -> (Uuid, Uuid, Uuid, Uuid) {
        (
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
        )
    }

    #[test]
    fn empty_lookup_is_none() {
        let t = ImdsBindingTable::new();
        assert!(t.is_empty());
        assert!(t.lookup(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))).is_none());
    }

    #[test]
    fn insert_then_lookup_returns_resolution() {
        let t = ImdsBindingTable::new();
        let (port, instance, _, _) = uuids();
        let pseudo = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 5));
        assert!(t.insert(pseudo, port, instance).is_none());
        assert_eq!(t.len(), 1);
        let r = t.lookup(pseudo).expect("registered");
        assert_eq!(r.port_id, port);
        assert_eq!(r.instance_id, instance);
    }

    #[test]
    fn insert_is_idempotent_returns_prior() {
        let t = ImdsBindingTable::new();
        let (port_a, instance_a, port_b, instance_b) = uuids();
        let pseudo = IpAddr::V6(Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 1));
        assert!(t.insert(pseudo, port_a, instance_a).is_none());
        let prior = t.insert(pseudo, port_b, instance_b).expect("first insert");
        assert_eq!(prior.port_id, port_a);
        assert_eq!(t.lookup(pseudo).unwrap().port_id, port_b);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn remove_by_port_cleans_all_for_that_port() {
        let t = ImdsBindingTable::new();
        let (port_a, instance_a, port_b, instance_b) = uuids();
        let p1 = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 1));
        let p2 = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 2));
        let p3 = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 3));
        t.insert(p1, port_a, instance_a);
        t.insert(p2, port_a, instance_a);
        t.insert(p3, port_b, instance_b);
        assert_eq!(t.remove_by_port(port_a), 2);
        assert!(t.lookup(p1).is_none());
        assert!(t.lookup(p2).is_none());
        assert_eq!(t.lookup(p3).unwrap().port_id, port_b);
    }

    #[test]
    fn remove_single_pseudo_src() {
        let t = ImdsBindingTable::new();
        let (port, instance, _, _) = uuids();
        let pseudo = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 5));
        t.insert(pseudo, port, instance);
        let r = t.remove(pseudo).expect("was present");
        assert_eq!(r.port_id, port);
        assert!(t.remove(pseudo).is_none());
        assert!(t.is_empty());
    }

    #[test]
    fn table_is_cheaply_cloneable_and_shared() {
        let a = ImdsBindingTable::new();
        let b = a.clone();
        let (port, instance, _, _) = uuids();
        let pseudo = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 9));
        a.insert(pseudo, port, instance);
        // Both clones see the insert (shared Arc<RwLock>).
        assert!(b.lookup(pseudo).is_some());
    }
}

// =============================================================================
// Blueprint integration
// =============================================================================

use tritond_client::types::{ImdsBindingWire, ProvisioningBlueprint};

/// Register every `ImdsBindingWire` entry from a `ProvisioningBlueprint`
/// in the table. Returns the count inserted. Idempotent: a re-apply
/// of the same blueprint overwrites prior entries for the same
/// `pseudo_src`. See `IMDS_DESIGN.md` §2.1.
///
/// Call this from the agent's `Provision` job handler **after**
/// `realize_provision_ports` returns successfully -- registering
/// bindings for a port that didn't actually start is pointless
/// and would orphan the entry on the next deploy.
pub fn register_blueprint_bindings(
    table: &ImdsBindingTable,
    blueprint: &ProvisioningBlueprint,
) -> usize {
    let mut n = 0;
    for b in &blueprint.imds_bindings {
        let pseudo: std::net::IpAddr = match b.pseudo_src.parse() {
            Ok(ip) => ip,
            Err(_) => {
                tracing::warn!(
                    pseudo_src = %b.pseudo_src,
                    port_id = %b.port_id,
                    "imds: skipping malformed pseudo_src in blueprint"
                );
                continue;
            }
        };
        table.insert(pseudo, b.port_id, b.instance_id);
        n += 1;
    }
    n
}

/// Drop every binding whose `port_id` is in `port_ids`. Used on
/// port-delete (the Stop/Restart paths leave bindings alone --
/// the port stays around). Returns the total count removed across
/// every port.
pub fn release_imds_bindings_for_ports(table: &ImdsBindingTable, port_ids: &[uuid::Uuid]) -> usize {
    let mut total = 0;
    for &port_id in port_ids {
        total += table.remove_by_port(port_id);
    }
    total
}

#[cfg(test)]
mod blueprint_tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use tritond_client::types::{JobKind, ProvisioningBlueprint as Bp};

    fn empty_bp() -> Bp {
        Bp {
            job_id: Uuid::new_v4(),
            kind: JobKind::Stop {
                instance_id: Uuid::new_v4(),
            },
            instance: None,
            image: None,
            nics: vec![],
            subnets: vec![],
            disks: vec![],
            ssh_public_keys: vec![],
            managed_identity: None,
            imds_bindings: vec![],
        }
    }

    #[test]
    fn empty_blueprint_is_a_noop() {
        let t = ImdsBindingTable::new();
        let bp = empty_bp();
        assert_eq!(register_blueprint_bindings(&t, &bp), 0);
        assert!(t.is_empty());
    }

    #[test]
    fn populated_blueprint_registers_each_entry() {
        let t = ImdsBindingTable::new();
        let mut bp = empty_bp();
        let port_a = Uuid::new_v4();
        let port_b = Uuid::new_v4();
        let instance = Uuid::new_v4();
        let pa = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 5));
        let pb = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 6));
        bp.imds_bindings = vec![
            ImdsBindingWire {
                pseudo_src: pa.to_string(),
                port_id: port_a,
                instance_id: instance,
            },
            ImdsBindingWire {
                pseudo_src: pb.to_string(),
                port_id: port_b,
                instance_id: instance,
            },
        ];
        assert_eq!(register_blueprint_bindings(&t, &bp), 2);
        assert_eq!(t.lookup(pa).unwrap().port_id, port_a);
        assert_eq!(t.lookup(pb).unwrap().port_id, port_b);
    }

    #[test]
    fn release_drops_only_matching_ports() {
        let t = ImdsBindingTable::new();
        let p1 = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 1));
        let p2 = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 2));
        let p3 = IpAddr::V4(Ipv4Addr::new(127, 1, 0, 3));
        let port_a = Uuid::new_v4();
        let port_b = Uuid::new_v4();
        let instance = Uuid::new_v4();
        t.insert(p1, port_a, instance);
        t.insert(p2, port_a, instance);
        t.insert(p3, port_b, instance);
        assert_eq!(release_imds_bindings_for_ports(&t, &[port_a]), 2);
        assert!(t.lookup(p3).is_some());
        assert!(t.lookup(p1).is_none());
    }
}
