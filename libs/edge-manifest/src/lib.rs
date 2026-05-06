// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared fhrun edge manifest schema.
//!
//! This crate intentionally mirrors `firehyve/tools/fhrun/src/manifest.rs`
//! so tritond can render the same JSON contract that fhrun consumes. The
//! backend field stays open for future values, but v1 renderers must emit
//! [`DATAPLANE_BACKEND_NFTABLES`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// v1 dataplane backend. AF_XDP remains reserved behind the same field.
pub const DATAPLANE_BACKEND_NFTABLES: &str = "nftables";

/// Reserved future dataplane backend value.
pub const DATAPLANE_BACKEND_AFXDP: &str = "afxdp";

/// v1 host-to-edge-agent control protocol name.
pub const EDGE_CONTROL_PROTOCOL_V1: &str = "triton.edge.control.v1";

/// Default guest device fhrun exposes for edge control.
pub const EDGE_CONTROL_GUEST_DEVICE: &str = "/dev/hvc0";

/// The user-supplied recipe for one fhrun invocation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Manifest {
    /// Human-readable firehyve VM name.
    pub name: String,
    /// Host path to the Linux ELF binary that runs inside the guest.
    pub bin: PathBuf,
    /// argv[1..] for the binary.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables passed to the binary.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory inside the guest.
    #[serde(default = "default_workdir")]
    pub workdir: String,
    /// Number of vCPUs.
    #[serde(default = "default_vcpus")]
    pub vcpus: usize,
    /// Memory size string parsed by firehyve.
    #[serde(default = "default_mem")]
    pub memory: String,
    /// Host path to the Linux kernel.
    pub kernel: PathBuf,
    /// Host path to the static fhrun-init binary.
    pub init: PathBuf,
    /// Extra files copied into the guest rootfs.
    #[serde(default)]
    pub extra_files: BTreeMap<String, PathBuf>,
    /// Single-NIC compatibility field.
    #[serde(default)]
    pub net: Option<NetConfig>,
    /// Multi-NIC attachments in eth0..ethN order.
    #[serde(default)]
    pub nics: Vec<NetConfig>,
    /// Dataplane intent interpreted by the edge-agent.
    #[serde(default)]
    pub dataplane: Option<DataplaneConfig>,
    /// Host/guest edge-control channel.
    #[serde(default)]
    pub edge_control: Option<EdgeControlConfig>,
    /// Path to the firehyve binary.
    #[serde(default = "default_firehyve")]
    pub firehyve: PathBuf,
    /// Optional kernel cmdline additions appended by fhrun.
    #[serde(default)]
    pub kernel_extra_cmdline: String,
}

/// One NIC. Becomes a viona attachment on the host and an `ethN`
/// interface inside the guest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NetConfig {
    /// Host vnic name.
    pub vnic: String,
    /// MAC address `xx:xx:xx:xx:xx:xx`.
    pub mac: String,
    /// In-guest IP address in CIDR form.
    pub ip: String,
    /// Optional default gateway.
    #[serde(default)]
    pub gateway: Option<String>,
    /// Operator-meaningful label such as `north` or `south`.
    #[serde(default)]
    pub role: Option<String>,
}

/// Dataplane intent. v1 acts on `snat` and `fips` via nftables.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DataplaneConfig {
    /// Dataplane backend selector.
    #[serde(default = "default_dataplane_backend")]
    pub backend: String,
    /// Source NAT rules.
    #[serde(default)]
    pub snat: Vec<SnatRule>,
    /// Floating IP 1:1 mappings.
    #[serde(default)]
    pub fips: Vec<FipMapping>,
    /// Reserved L4 load balancer intents.
    #[serde(default)]
    pub load_balancers: Vec<LoadBalancer>,
    /// Reserved BGP speaker config.
    #[serde(default)]
    pub bgp: Option<BgpConfig>,
    /// Optional in-guest TCP debug listener.
    #[serde(default)]
    pub control_listen: Option<String>,
}

/// Host/guest control channel for edge-agent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct EdgeControlConfig {
    /// Host Unix socket path. If absent, fhrun generates one under
    /// its runtime directory.
    #[serde(default)]
    pub socket: Option<PathBuf>,
    /// Guest device path where edge-agent reads/writes the stream.
    #[serde(default = "default_edge_control_guest_device")]
    pub guest_device: String,
    /// Line-oriented JSON protocol name spoken on this stream.
    #[serde(default = "default_edge_control_protocol")]
    pub protocol: String,
}

/// Edge-control subset handed to the guest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct EdgeControlGuestConfig {
    /// Guest control stream device.
    pub guest_device: String,
    /// Control protocol name.
    pub protocol: String,
}

/// Source NAT rule.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SnatRule {
    /// Source CIDR or single IP that the rule matches.
    pub from: String,
    /// Replacement source IP.
    pub via: String,
}

/// 1:1 floating IP mapping.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FipMapping {
    /// Public-facing IP.
    pub external: String,
    /// Tenant/private-side IP.
    pub internal: String,
}

/// Reserved L4 load balancer intent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LoadBalancer {
    /// Virtual IP + port.
    pub vip: String,
    /// Backend endpoints.
    pub backends: Vec<String>,
    /// Steering algorithm.
    #[serde(default = "default_lb_algo")]
    pub algorithm: String,
    /// Reserved health-check spec.
    #[serde(default)]
    pub health_check: Option<String>,
}

/// Reserved BGP speaker config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BgpConfig {
    /// Local AS number.
    pub asn: u32,
    /// Router ID.
    pub router_id: String,
    /// eBGP peers.
    #[serde(default)]
    pub peers: Vec<BgpPeer>,
    /// Prefixes to announce.
    #[serde(default)]
    pub announce: Vec<String>,
}

/// Reserved eBGP peer config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BgpPeer {
    /// Peer IP.
    pub ip: String,
    /// Peer AS number.
    pub asn: u32,
}

/// Runtime-relevant guest spec derived from a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GuestSpec {
    /// In-guest binary path.
    pub bin: String,
    /// argv[1..] for the binary.
    pub args: Vec<String>,
    /// Environment variables.
    pub env: BTreeMap<String, String>,
    /// Working directory.
    pub workdir: String,
    /// All NICs in eth0..ethN order.
    #[serde(default)]
    pub nics: Vec<NetConfig>,
    /// Dataplane intent.
    #[serde(default)]
    pub dataplane: Option<DataplaneConfig>,
    /// Edge-agent control stream.
    #[serde(default)]
    pub edge_control: Option<EdgeControlGuestConfig>,
}

impl Manifest {
    /// Iterate every NIC in declaration order: legacy `net` first,
    /// then `nics`.
    pub fn all_nics(&self) -> impl Iterator<Item = &NetConfig> {
        self.net.iter().chain(self.nics.iter())
    }

    /// Return whether fhrun should create an edge-control channel.
    #[must_use]
    pub fn edge_control_enabled(&self) -> bool {
        self.edge_control.is_some() || self.dataplane.is_some()
    }

    /// Resolve the host-side edge-control socket path.
    #[must_use]
    pub fn edge_control_socket_path(&self, runtime_dir: &Path) -> Option<PathBuf> {
        if !self.edge_control_enabled() {
            return None;
        }
        Some(
            self.edge_control
                .as_ref()
                .and_then(|control| control.socket.clone())
                .unwrap_or_else(|| runtime_dir.join("edge-control.sock")),
        )
    }

    /// Project the manifest down to the in-guest spec.
    #[must_use]
    pub fn to_guest_spec(&self, in_guest_bin: &str) -> GuestSpec {
        GuestSpec {
            bin: in_guest_bin.to_string(),
            args: self.args.clone(),
            env: self.env.clone(),
            workdir: self.workdir.clone(),
            nics: self.all_nics().cloned().collect(),
            dataplane: self.dataplane.clone(),
            edge_control: self.edge_control_guest_spec(),
        }
    }

    fn edge_control_guest_spec(&self) -> Option<EdgeControlGuestConfig> {
        if !self.edge_control_enabled() {
            return None;
        }
        let cfg = self.edge_control.as_ref();
        Some(EdgeControlGuestConfig {
            guest_device: cfg
                .map(|control| control.guest_device.clone())
                .unwrap_or_else(default_edge_control_guest_device),
            protocol: cfg
                .map(|control| control.protocol.clone())
                .unwrap_or_else(default_edge_control_protocol),
        })
    }
}

fn default_workdir() -> String {
    "/".to_string()
}

fn default_vcpus() -> usize {
    1
}

fn default_mem() -> String {
    "128M".to_string()
}

fn default_firehyve() -> PathBuf {
    PathBuf::from("firehyve")
}

fn default_dataplane_backend() -> String {
    DATAPLANE_BACKEND_NFTABLES.to_string()
}

fn default_lb_algo() -> String {
    "maglev".to_string()
}

fn default_edge_control_guest_device() -> String {
    EDGE_CONTROL_GUEST_DEVICE.to_string()
}

fn default_edge_control_protocol() -> String {
    EDGE_CONTROL_PROTOCOL_V1.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_serializes_fhrun_edge_contract() {
        let manifest = Manifest {
            name: "triton-edge-example".to_string(),
            bin: PathBuf::from("/opt/firehyve/bin/edge-agent"),
            args: Vec::new(),
            env: BTreeMap::new(),
            workdir: "/".to_string(),
            vcpus: 1,
            memory: "128M".to_string(),
            kernel: PathBuf::from("/opt/firehyve/kernels/v1/bzImage"),
            init: PathBuf::from("/opt/firehyve/bin/fhrun-init"),
            extra_files: BTreeMap::new(),
            net: None,
            nics: vec![
                NetConfig {
                    vnic: "edge0_north".to_string(),
                    mac: "02:00:00:00:00:10".to_string(),
                    ip: "192.0.2.10/24".to_string(),
                    gateway: Some("192.0.2.1".to_string()),
                    role: Some("north".to_string()),
                },
                NetConfig {
                    vnic: "edge0_south".to_string(),
                    mac: "02:00:00:00:00:11".to_string(),
                    ip: "10.0.0.2/24".to_string(),
                    gateway: None,
                    role: Some("south".to_string()),
                },
            ],
            dataplane: Some(DataplaneConfig {
                backend: DATAPLANE_BACKEND_NFTABLES.to_string(),
                snat: vec![SnatRule {
                    from: "10.0.1.0/24".to_string(),
                    via: "203.0.113.1".to_string(),
                }],
                fips: vec![FipMapping {
                    external: "203.0.113.2".to_string(),
                    internal: "10.0.1.20".to_string(),
                }],
                load_balancers: Vec::new(),
                bgp: None,
                control_listen: None,
            }),
            edge_control: Some(EdgeControlConfig {
                socket: Some(PathBuf::from(
                    "/var/lib/tritonagent/edge/edge0/edge-control.sock",
                )),
                guest_device: EDGE_CONTROL_GUEST_DEVICE.to_string(),
                protocol: EDGE_CONTROL_PROTOCOL_V1.to_string(),
            }),
            firehyve: PathBuf::from("/opt/firehyve/bin/firehyve"),
            kernel_extra_cmdline: String::new(),
        };

        let actual = match serde_json::to_value(&manifest) {
            Ok(value) => value,
            Err(err) => panic!("serialize manifest: {err}"),
        };
        assert_eq!(
            actual,
            json!({
                "name": "triton-edge-example",
                "bin": "/opt/firehyve/bin/edge-agent",
                "args": [],
                "env": {},
                "workdir": "/",
                "vcpus": 1,
                "memory": "128M",
                "kernel": "/opt/firehyve/kernels/v1/bzImage",
                "init": "/opt/firehyve/bin/fhrun-init",
                "extra_files": {},
                "net": null,
                "nics": [
                    {
                        "vnic": "edge0_north",
                        "mac": "02:00:00:00:00:10",
                        "ip": "192.0.2.10/24",
                        "gateway": "192.0.2.1",
                        "role": "north"
                    },
                    {
                        "vnic": "edge0_south",
                        "mac": "02:00:00:00:00:11",
                        "ip": "10.0.0.2/24",
                        "gateway": null,
                        "role": "south"
                    }
                ],
                "dataplane": {
                    "backend": "nftables",
                    "snat": [
                        {
                            "from": "10.0.1.0/24",
                            "via": "203.0.113.1"
                        }
                    ],
                    "fips": [
                        {
                            "external": "203.0.113.2",
                            "internal": "10.0.1.20"
                        }
                    ],
                    "load_balancers": [],
                    "bgp": null,
                    "control_listen": null
                },
                "edge_control": {
                    "socket": "/var/lib/tritonagent/edge/edge0/edge-control.sock",
                    "guest_device": "/dev/hvc0",
                    "protocol": "triton.edge.control.v1"
                },
                "firehyve": "/opt/firehyve/bin/firehyve",
                "kernel_extra_cmdline": ""
            })
        );
    }

    #[test]
    fn dataplane_implies_edge_control_guest_spec() {
        let manifest = Manifest {
            name: "triton-edge-example".to_string(),
            bin: PathBuf::from("/edge-agent"),
            args: Vec::new(),
            env: BTreeMap::new(),
            workdir: "/".to_string(),
            vcpus: 1,
            memory: "128M".to_string(),
            kernel: PathBuf::from("/kernel"),
            init: PathBuf::from("/init"),
            extra_files: BTreeMap::new(),
            net: None,
            nics: Vec::new(),
            dataplane: Some(DataplaneConfig {
                backend: DATAPLANE_BACKEND_NFTABLES.to_string(),
                snat: Vec::new(),
                fips: Vec::new(),
                load_balancers: Vec::new(),
                bgp: None,
                control_listen: None,
            }),
            edge_control: None,
            firehyve: PathBuf::from("firehyve"),
            kernel_extra_cmdline: String::new(),
        };

        let guest = manifest.to_guest_spec("/edge-agent");
        assert_eq!(
            guest.edge_control,
            Some(EdgeControlGuestConfig {
                guest_device: EDGE_CONTROL_GUEST_DEVICE.to_string(),
                protocol: EDGE_CONTROL_PROTOCOL_V1.to_string(),
            })
        );
        assert_eq!(
            manifest.edge_control_socket_path(Path::new("/tmp/runtime")),
            Some(PathBuf::from("/tmp/runtime/edge-control.sock"))
        );
    }
}
