// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! IMDS dataplane wiring config.
//!
//! Two CN-wide knobs (sourced from env vars):
//!   * `TRITOND_IMDS_LISTENER_IPV4` -- address the per-CN proteus-owned
//!     IMDS listener binds to. The kmod DNATs `169.254.169.254/32`
//!     flows here. Setting it enables the IMDS dataplane wire across
//!     the cluster; unset means tritond doesn't add `LocalImds` routes
//!     or `ImdsBinding`s to port blueprints, and the dataplane stays
//!     completely inert.
//!   * `TRITOND_IMDS_LISTENER_PORT` -- TCP port the listener accepts
//!     on (default `8051`; production binds 80 on the dedicated
//!     datalink, dev binds something high on localhost).
//!
//! The realized `config/imds/enabled` (layered metadata) still gates
//! per-instance: even with both env vars set, an instance whose
//! realized view says `imds_enabled == false` gets no `LocalImds`
//! route and no binding (so the listener never serves it). See
//! `IMDS_DESIGN.md` §3 + §1.5.
//!
//! Per-port pseudo source comes from this module's
//! [`pseudo_src_for_port`]: `100.64.x.y` derived from the low 16 bits
//! of the port uuid. CGNAT space (RFC 6598) avoids collision with any
//! realistic guest / underlay range; 65k unique values per CN is well
//! above the per-port population cap.

use std::env;
use std::net::{IpAddr, Ipv4Addr};

use uuid::Uuid;

/// CN-wide IMDS listener config sourced from env vars. `None` means
/// IMDS is not wired in this cluster; tritond emits port blueprints
/// with `bp.imds = None` and no `LocalImds` route entries, and the
/// dataplane behaves exactly as it did before the IM-3 brick landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImdsListenerConfig {
    pub listener_ip: IpAddr,
    pub listener_port: u16,
}

impl ImdsListenerConfig {
    /// Resolve the effective config from the process environment. Run
    /// once at startup -- the result is immutable for the lifetime of
    /// the process (matches the rest of the bootstrap-config story:
    /// changing one requires a restart).
    pub fn from_env() -> Option<Self> {
        let listener_ip = env::var("TRITOND_IMDS_LISTENER_IPV4").ok()?;
        let listener_ip: IpAddr = listener_ip.trim().parse().ok()?;
        let listener_port = env::var("TRITOND_IMDS_LISTENER_PORT")
            .ok()
            .and_then(|v| v.trim().parse::<u16>().ok())
            .unwrap_or(DEFAULT_IMDS_LISTENER_PORT);
        if listener_port == 0 {
            return None;
        }
        Some(Self {
            listener_ip,
            listener_port,
        })
    }
}

/// Dev default. Production-ish builds should set
/// `TRITOND_IMDS_LISTENER_PORT=80` on the dedicated datalink path; dev
/// binds something high on localhost (`8051` matches the
/// `--imds-listen-addr` examples in `tritonagent` docs).
pub const DEFAULT_IMDS_LISTENER_PORT: u16 = 8051;

/// Compute the per-port pseudo source address. The kmod's IMDS NAT
/// rewrites outbound IMDS flows' source to this so the listener
/// `accept()`s a connection whose peer address is uniquely the
/// originating port. We pick a deterministic CN-unique address from
/// the port UUID so re-applying the same blueprint after a restart
/// stays bit-identical (and so the agent's binding table can be
/// rebuilt purely from blueprint data on reconnect).
///
/// `100.64.0.0/16` is CGNAT space; `100.64.{0..255}.{0..255}` gives
/// 65,536 unique pseudo-sources per CN, comfortably above any per-CN
/// port population. The high octet pair is derived from the UUID's
/// last two bytes, the low pair from the two before that -- using a
/// large window inside the UUID rather than `as_u128() & 0xFFFF` so
/// adjacent port creates spread out instead of colliding when the
/// caller hands out sequential UUIDs (some test paths do).
pub fn pseudo_src_for_port(port_id: Uuid) -> IpAddr {
    let bytes = port_id.as_bytes();
    let lo = bytes[15];
    let mid = bytes[14];
    IpAddr::V4(Ipv4Addr::new(100, 64, mid, lo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudo_src_uses_last_two_bytes_of_uuid() {
        let id = Uuid::from_u128(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        let IpAddr::V4(v4) = pseudo_src_for_port(id) else {
            panic!("expected v4");
        };
        // bytes[14]=0x66, bytes[15]=0x77
        assert_eq!(v4.octets(), [100, 64, 0x66, 0x77]);
    }

    #[test]
    fn pseudo_src_is_stable_across_calls() {
        let id = Uuid::new_v4();
        assert_eq!(pseudo_src_for_port(id), pseudo_src_for_port(id));
    }

    #[test]
    fn pseudo_src_differs_per_port() {
        let a = Uuid::from_u128(0x1);
        let b = Uuid::from_u128(0x2);
        assert_ne!(pseudo_src_for_port(a), pseudo_src_for_port(b));
    }
}
