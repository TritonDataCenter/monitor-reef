// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Pure renderer for firehyve/fhrun north/south edge manifests.
//!
//! This module does no store reads and no I/O. The edge materializer owns
//! placement, route/subnet selection, and FIP binding resolution; this module
//! only projects that desired state into the shared fhrun manifest contract.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;

use edge_manifest::{
    DATAPLANE_BACKEND_NFTABLES, DataplaneConfig, EDGE_CONTROL_GUEST_DEVICE,
    EDGE_CONTROL_PROTOCOL_V1, EdgeControlConfig, FipMapping, Manifest, NetConfig, SnatRule,
};
use tritond_api::types::{FloatingIp, NatGateway};
use uuid::Uuid;

/// fhrun role label for the public-side NIC.
pub const EDGE_NIC_ROLE_NORTH: &str = "north";

/// fhrun role label for the tenant/VPC-side NIC.
pub const EDGE_NIC_ROLE_SOUTH: &str = "south";

/// Fully-resolved fhrun placement for one edge instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeManifestPlacement {
    /// Stable edge instance id. The renderer derives the firehyve VM
    /// name from this id.
    pub edge_instance_id: Uuid,
    /// Host path to the firehyve binary.
    pub firehyve: PathBuf,
    /// Host path to the Linux edge kernel.
    pub kernel: PathBuf,
    /// Host path to fhrun-init.
    pub init: PathBuf,
    /// Host path to the edge-agent binary copied into the guest rootfs.
    pub edge_agent_bin: PathBuf,
    /// Host Unix socket bridged into the guest as the edge-control stream.
    pub edge_control_socket: PathBuf,
    /// Public-side NIC.
    pub north_nic: EdgeNicPlacement,
    /// Tenant/VPC-side NIC.
    pub south_nic: EdgeNicPlacement,
    /// Edge VM vCPU count.
    pub vcpus: usize,
    /// Edge VM memory string parsed by firehyve, e.g. `128M`.
    pub memory: String,
}

/// Resolved host and guest addressing for one edge NIC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeNicPlacement {
    /// Host vnic name resolved by fhrun/firehyve.
    pub vnic: String,
    /// Guest MAC address.
    pub mac: String,
    /// Guest IP address in CIDR form.
    pub ip: String,
    /// Optional default gateway.
    pub gateway: Option<String>,
}

/// Route and FIP bindings that this edge instance should realize.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EdgeManifestBindings {
    /// Tenant source CIDRs that SNAT through `NatGateway.public_address`.
    pub snat_sources: Vec<String>,
    /// Floating IP bindings in this edge cluster.
    pub floating_ips: Vec<EdgeFloatingIpBinding>,
}

/// Whether a floating IP is rewritten by Proteus on the tenant CN or
/// by the north/south edge dataplane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatingIpTermination {
    /// Existing v1 default: Proteus handles the FIP rewrite on the CN.
    CnTerminated,
    /// Reserved edge-side termination: fhrun edge-agent receives a
    /// dataplane FIP mapping.
    EdgeTerminated,
}

/// Resolved floating IP binding for an edge render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeFloatingIpBinding {
    /// Floating IP resource id.
    pub floating_ip_id: Uuid,
    /// Public-facing floating IP address.
    pub external_ip: IpAddr,
    /// Tenant-side address for the attached NIC.
    pub internal_ip: IpAddr,
    /// Where the rewrite is realized.
    pub termination: FloatingIpTermination,
}

impl EdgeFloatingIpBinding {
    /// Create a CN-terminated binding from a stored floating IP.
    #[must_use]
    pub fn cn_terminated(fip: &FloatingIp, internal_ip: IpAddr) -> Self {
        Self::from_floating_ip(fip, internal_ip, FloatingIpTermination::CnTerminated)
    }

    /// Create an edge-terminated binding from a stored floating IP.
    #[must_use]
    pub fn edge_terminated(fip: &FloatingIp, internal_ip: IpAddr) -> Self {
        Self::from_floating_ip(fip, internal_ip, FloatingIpTermination::EdgeTerminated)
    }

    fn from_floating_ip(
        fip: &FloatingIp,
        internal_ip: IpAddr,
        termination: FloatingIpTermination,
    ) -> Self {
        Self {
            floating_ip_id: fip.id,
            external_ip: fip.address,
            internal_ip,
            termination,
        }
    }
}

/// Render a v1 fhrun manifest for one NAT edge instance.
#[must_use]
pub fn render_edge_manifest(
    nat_gateway: &NatGateway,
    bindings: &EdgeManifestBindings,
    placement: &EdgeManifestPlacement,
) -> Manifest {
    Manifest {
        name: edge_instance_name(placement.edge_instance_id),
        bin: placement.edge_agent_bin.clone(),
        args: Vec::new(),
        env: BTreeMap::new(),
        workdir: "/".to_string(),
        vcpus: placement.vcpus,
        memory: placement.memory.clone(),
        kernel: placement.kernel.clone(),
        init: placement.init.clone(),
        extra_files: BTreeMap::new(),
        net: None,
        nics: vec![
            render_nic(&placement.north_nic, EDGE_NIC_ROLE_NORTH),
            render_nic(&placement.south_nic, EDGE_NIC_ROLE_SOUTH),
        ],
        dataplane: Some(render_dataplane(nat_gateway, bindings)),
        edge_control: Some(EdgeControlConfig {
            socket: Some(placement.edge_control_socket.clone()),
            guest_device: EDGE_CONTROL_GUEST_DEVICE.to_string(),
            protocol: EDGE_CONTROL_PROTOCOL_V1.to_string(),
        }),
        firehyve: placement.firehyve.clone(),
        kernel_extra_cmdline: String::new(),
    }
}

fn edge_instance_name(edge_instance_id: Uuid) -> String {
    format!("triton-edge-{}", edge_instance_id.simple())
}

fn render_nic(nic: &EdgeNicPlacement, role: &str) -> NetConfig {
    NetConfig {
        vnic: nic.vnic.clone(),
        mac: nic.mac.clone(),
        ip: nic.ip.clone(),
        gateway: nic.gateway.clone(),
        role: Some(role.to_string()),
    }
}

fn render_dataplane(nat_gateway: &NatGateway, bindings: &EdgeManifestBindings) -> DataplaneConfig {
    DataplaneConfig {
        backend: DATAPLANE_BACKEND_NFTABLES.to_string(),
        snat: bindings
            .snat_sources
            .iter()
            .map(|source| SnatRule {
                from: source.clone(),
                via: nat_gateway.public_address.to_string(),
            })
            .collect(),
        fips: bindings
            .floating_ips
            .iter()
            .filter(|binding| matches!(binding.termination, FloatingIpTermination::EdgeTerminated))
            .map(|binding| FipMapping {
                external: binding.external_ip.to_string(),
                internal: binding.internal_ip.to_string(),
            })
            .collect(),
        load_balancers: Vec::new(),
        bgp: None,
        control_listen: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    use chrono::{DateTime, Utc};
    use serde_json::{Value, json};
    use tritond_api::types::{AddressFamily, FloatingIpAttachment};
    use tritond_store::RealizedNetworkState;

    #[test]
    fn renders_nat_gateway_with_zero_fips() {
        let nat_gateway = nat_gateway();
        let bindings = EdgeManifestBindings {
            snat_sources: vec!["10.0.1.0/24".to_string()],
            floating_ips: Vec::new(),
        };

        let manifest = render_edge_manifest(&nat_gateway, &bindings, &placement());

        assert_eq!(
            manifest_json(&manifest),
            expected_manifest_json(
                json!([
                    {
                        "from": "10.0.1.0/24",
                        "via": "203.0.113.10"
                    }
                ]),
                json!([])
            )
        );
    }

    #[test]
    fn renders_cn_terminated_fip_without_edge_mapping() {
        let nat_gateway = nat_gateway();
        let fip = floating_ip(Ipv4Addr::new(203, 0, 113, 20));
        let bindings = EdgeManifestBindings {
            snat_sources: vec!["10.0.1.0/24".to_string()],
            floating_ips: vec![EdgeFloatingIpBinding::cn_terminated(
                &fip,
                IpAddr::V4(Ipv4Addr::new(10, 0, 1, 20)),
            )],
        };

        let manifest = render_edge_manifest(&nat_gateway, &bindings, &placement());

        assert_eq!(
            manifest_json(&manifest),
            expected_manifest_json(
                json!([
                    {
                        "from": "10.0.1.0/24",
                        "via": "203.0.113.10"
                    }
                ]),
                json!([])
            )
        );
    }

    #[test]
    fn renders_edge_terminated_fip_mapping() {
        let nat_gateway = nat_gateway();
        let fip = floating_ip(Ipv4Addr::new(203, 0, 113, 20));
        let bindings = EdgeManifestBindings {
            snat_sources: vec!["10.0.1.0/24".to_string()],
            floating_ips: vec![EdgeFloatingIpBinding::edge_terminated(
                &fip,
                IpAddr::V4(Ipv4Addr::new(10, 0, 1, 20)),
            )],
        };

        let manifest = render_edge_manifest(&nat_gateway, &bindings, &placement());

        assert_eq!(
            manifest_json(&manifest),
            expected_manifest_json(
                json!([
                    {
                        "from": "10.0.1.0/24",
                        "via": "203.0.113.10"
                    }
                ]),
                json!([
                    {
                        "external": "203.0.113.20",
                        "internal": "10.0.1.20"
                    }
                ])
            )
        );
    }

    fn nat_gateway() -> NatGateway {
        let now = test_time();
        NatGateway {
            id: uuid(0x0100),
            tenant_id: uuid(0x0101),
            project_id: uuid(0x0102),
            vpc_id: uuid(0x0103),
            name: "egress".to_string(),
            description: String::new(),
            family: AddressFamily::V4,
            public_address: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
            edge_cluster_id: None,
            desired_generation: 1,
            realized: RealizedNetworkState::from_rows(1, Vec::new()),
            created_at: now,
            updated_at: now,
        }
    }

    fn floating_ip(address: Ipv4Addr) -> FloatingIp {
        let now = test_time();
        FloatingIp {
            id: uuid(0x0200),
            tenant_id: uuid(0x0101),
            project_id: uuid(0x0102),
            name: "ssh".to_string(),
            description: String::new(),
            address: IpAddr::V4(address),
            attached_to: Some(FloatingIpAttachment {
                instance_id: uuid(0x0300),
                nic_id: uuid(0x0301),
                attached_at: now,
            }),
            network_id: None,
            external_nic_tag: None,
            hosted_cn: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn placement() -> EdgeManifestPlacement {
        EdgeManifestPlacement {
            edge_instance_id: uuid(0x0e1),
            firehyve: PathBuf::from("/opt/firehyve/bin/firehyve"),
            kernel: PathBuf::from("/opt/firehyve/kernels/linux-v1/bzImage"),
            init: PathBuf::from("/opt/firehyve/bin/fhrun-init"),
            edge_agent_bin: PathBuf::from("/opt/firehyve/bin/edge-agent"),
            edge_control_socket: PathBuf::from(
                "/var/lib/tritonagent/edge/00000000-0000-0000-0000-0000000000e1/edge-control.sock",
            ),
            north_nic: EdgeNicPlacement {
                vnic: "edge-e1-north".to_string(),
                mac: "02:00:00:00:0e:10".to_string(),
                ip: "192.0.2.10/24".to_string(),
                gateway: Some("192.0.2.1".to_string()),
            },
            south_nic: EdgeNicPlacement {
                vnic: "edge-e1-south".to_string(),
                mac: "02:00:00:00:0e:11".to_string(),
                ip: "10.0.0.2/24".to_string(),
                gateway: None,
            },
            vcpus: 1,
            memory: "128M".to_string(),
        }
    }

    fn expected_manifest_json(snat: Value, fips: Value) -> Value {
        json!({
            "name": "triton-edge-000000000000000000000000000000e1",
            "bin": "/opt/firehyve/bin/edge-agent",
            "args": [],
            "env": {},
            "workdir": "/",
            "vcpus": 1,
            "memory": "128M",
            "kernel": "/opt/firehyve/kernels/linux-v1/bzImage",
            "init": "/opt/firehyve/bin/fhrun-init",
            "extra_files": {},
            "net": null,
            "nics": [
                {
                    "vnic": "edge-e1-north",
                    "mac": "02:00:00:00:0e:10",
                    "ip": "192.0.2.10/24",
                    "gateway": "192.0.2.1",
                    "role": "north"
                },
                {
                    "vnic": "edge-e1-south",
                    "mac": "02:00:00:00:0e:11",
                    "ip": "10.0.0.2/24",
                    "gateway": null,
                    "role": "south"
                }
            ],
            "dataplane": {
                "backend": "nftables",
                "snat": snat,
                "fips": fips,
                "load_balancers": [],
                "bgp": null,
                "control_listen": null
            },
            "edge_control": {
                "socket": "/var/lib/tritonagent/edge/00000000-0000-0000-0000-0000000000e1/edge-control.sock",
                "guest_device": "/dev/hvc0",
                "protocol": "triton.edge.control.v1"
            },
            "firehyve": "/opt/firehyve/bin/firehyve",
            "kernel_extra_cmdline": ""
        })
    }

    fn manifest_json(manifest: &Manifest) -> Value {
        match serde_json::to_value(manifest) {
            Ok(value) => value,
            Err(err) => panic!("serialize manifest: {err}"),
        }
    }

    fn test_time() -> DateTime<Utc> {
        match DateTime::from_timestamp(0, 0) {
            Some(time) => time,
            None => panic!("unix epoch must be representable"),
        }
    }

    fn uuid(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }
}
