// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Network patch generation for Talos
//!
//! This module provides functionality to convert Triton NIC data into Talos
//! network configuration patches. The generated YAML patches can be applied
//! to Talos nodes to persist their network configuration.
//!
//! # Example
//!
//! ```no_run
//! use cloudapi_api::types::network::Nic;
//! use triton::commands::k8s::network::generate_network_patch;
//!
//! # fn main() -> anyhow::Result<()> {
//! // Assume we got these NICs from: triton instance nic list -j <instance-id>
//! let nics = vec![
//!     Nic {
//!         mac: "90:b8:d0:2f:1a:62".to_string(),
//!         primary: true,
//!         ip: "192.168.129.200".to_string(),
//!         netmask: "255.255.248.0".to_string(),
//!         gateway: Some("192.168.128.1".to_string()),
//!         network: "12345678-1234-1234-1234-123456789012".parse()?,
//!         state: None,
//!     }
//! ];
//!
//! let nameservers = vec!["8.8.8.8".to_string()];
//!
//! // Generate network patch for a worker node (None = use primary NIC subnet)
//! let yaml = generate_network_patch(&nics, &nameservers, false, None)?;
//!
//! // Write to file and apply with: talosctl apply-config --file <file>
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result, anyhow, bail};
use cloudapi_api::types::network::Nic;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Talos machine network configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct TalosNetworkPatch {
    pub machine: MachineConfig,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<ClusterConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MachineConfig {
    pub kubelet: KubeletConfig,

    pub network: NetworkConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KubeletConfig {
    #[serde(rename = "nodeIP")]
    pub node_ip: NodeIpConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeIpConfig {
    #[serde(rename = "validSubnets")]
    pub valid_subnets: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub nameservers: Vec<String>,

    pub interfaces: Vec<InterfaceConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InterfaceConfig {
    #[serde(rename = "deviceSelector")]
    pub device_selector: DeviceSelector,

    pub addresses: Vec<String>,

    pub routes: Vec<RouteConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceSelector {
    #[serde(rename = "hardwareAddr")]
    pub hardware_addr: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RouteConfig {
    pub network: String,

    pub gateway: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub etcd: EtcdConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EtcdConfig {
    #[serde(rename = "advertisedSubnets")]
    pub advertised_subnets: Vec<String>,
}

/// Convert a dotted decimal netmask (e.g. 255.255.248.0) to CIDR prefix (e.g. 21)
///
/// # Arguments
/// * `netmask` - Dotted decimal netmask string (e.g. "255.255.248.0")
///
/// # Returns
/// CIDR prefix length (e.g. 21 for /21)
///
/// # Errors
/// Returns an error if:
/// - The netmask cannot be parsed as an IPv4 address
/// - The netmask is invalid (has zeros before ones)
pub fn netmask_to_prefix(netmask: &str) -> Result<u8> {
    let addr: Ipv4Addr = netmask
        .parse()
        .context("failed to parse netmask as IPv4 address")?;

    let octets = addr.octets();
    let mask_bits = u32::from_be_bytes(octets);

    // Count leading ones
    // A valid netmask has all 1s on the left, all 0s on the right
    let prefix = mask_bits.leading_ones();

    // Validate it's a proper netmask (no 0s before 1s)
    if mask_bits != (!0u32).checked_shl(32 - prefix).unwrap_or(0) {
        bail!("invalid netmask: {} has zeros before ones", netmask);
    }

    Ok(prefix as u8)
}

/// Calculate network address from IP and netmask
///
/// # Arguments
/// * `ip` - IP address string
/// * `netmask` - Netmask string
///
/// # Returns
/// Network address with CIDR prefix (e.g. "192.168.128.0/21")
///
/// # Errors
/// Returns an error if:
/// - The IP address cannot be parsed
/// - The netmask cannot be parsed or is invalid
pub fn calculate_network_cidr(ip: &str, netmask: &str) -> Result<String> {
    let ip_addr: Ipv4Addr = ip.parse().context("failed to parse IP address")?;
    let netmask_addr: Ipv4Addr = netmask.parse().context("failed to parse netmask")?;

    let ip_bits = u32::from_be_bytes(ip_addr.octets());
    let mask_bits = u32::from_be_bytes(netmask_addr.octets());

    let network_bits = ip_bits & mask_bits;
    let network_addr = Ipv4Addr::from(network_bits);

    let prefix = netmask_to_prefix(netmask)?;

    Ok(format!("{}/{}", network_addr, prefix))
}

/// Generate a Talos network patch from Triton NIC data
///
/// Converts Triton NIC information into a Talos-compatible network
/// configuration patch that can be applied to persist networking across
/// reboots and upgrades.
///
/// # Arguments
/// * `nics` - Array of NICs from Triton (from `triton instance nic list -j`)
/// * `nameservers` - DNS nameservers to use
/// * `is_control_plane` - Whether this is a control plane node (adds etcd config)
/// * `fabric_network_id` - UUID of the fabric network (used to determine which
///   subnet to use for kubelet nodeIP and etcd advertisedSubnets)
///
/// # Returns
/// Talos network configuration patch as YAML string
///
/// # Network Selection Logic
///
/// For dual-homed control plane nodes (e.g. with both external and fabric NICs),
/// the kubelet nodeIP and etcd advertisedSubnets must use the **fabric network**
/// subnet. This ensures workers on the fabric network can reach the control plane's
/// Kubernetes API via ClusterIP routing.
///
/// If `fabric_network_id` is provided and matches a NIC, that NIC's subnet is used.
/// Otherwise, falls back to the primary NIC's subnet.
///
/// # Errors
/// Returns an error if:
/// - No NICs are provided
/// - No primary NIC is found
/// - Any NIC is missing a gateway
/// - Netmask conversion fails
pub fn generate_network_patch(
    nics: &[Nic],
    nameservers: &[String],
    is_control_plane: bool,
    fabric_network_id: Option<uuid::Uuid>,
) -> Result<String> {
    if nics.is_empty() {
        bail!("no NICs provided");
    }

    // Find primary NIC for fallback
    let primary_nic = nics
        .iter()
        .find(|n| n.primary)
        .ok_or_else(|| anyhow!("no primary NIC found"))?;

    // For kubelet nodeIP and etcd advertisedSubnets, prefer the fabric network
    // if specified. This is critical for dual-homed control plane nodes where
    // the external IP is used for management but the fabric IP must be used
    // for intra-cluster communication.
    let node_ip_nic = if let Some(fabric_id) = fabric_network_id {
        nics.iter()
            .find(|n| n.network == fabric_id)
            .unwrap_or(primary_nic)
    } else {
        primary_nic
    };

    // Calculate network CIDR from the selected NIC
    let network_cidr = calculate_network_cidr(&node_ip_nic.ip, &node_ip_nic.netmask)?;

    let mut interfaces = Vec::new();

    for nic in nics {
        let prefix = netmask_to_prefix(&nic.netmask)?;

        // Determine default route
        // IPv4: 0.0.0.0/0
        // IPv6: ::/0
        let default_route = if nic.ip.contains(':') {
            "::/0"
        } else {
            "0.0.0.0/0"
        };

        let gateway = nic
            .gateway
            .as_ref()
            .ok_or_else(|| anyhow!("NIC {} missing gateway", nic.mac))?;

        interfaces.push(InterfaceConfig {
            device_selector: DeviceSelector {
                hardware_addr: nic.mac.clone(),
            },
            addresses: vec![format!("{}/{}", nic.ip, prefix)],
            routes: vec![RouteConfig {
                network: default_route.to_string(),
                gateway: gateway.clone(),
            }],
        });
    }

    let patch = TalosNetworkPatch {
        machine: MachineConfig {
            kubelet: KubeletConfig {
                node_ip: NodeIpConfig {
                    valid_subnets: vec![network_cidr.clone()],
                },
            },
            network: NetworkConfig {
                nameservers: nameservers.to_vec(),
                interfaces,
            },
        },
        cluster: if is_control_plane {
            Some(ClusterConfig {
                etcd: EtcdConfig {
                    advertised_subnets: vec![network_cidr],
                },
            })
        } else {
            None
        },
    };

    serde_yaml::to_string(&patch).context("failed to serialize network patch to YAML")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_netmask_to_prefix() {
        assert_eq!(netmask_to_prefix("255.255.255.0").unwrap(), 24);
        assert_eq!(netmask_to_prefix("255.255.248.0").unwrap(), 21);
        assert_eq!(netmask_to_prefix("255.255.252.0").unwrap(), 22);
        assert_eq!(netmask_to_prefix("255.255.0.0").unwrap(), 16);
        assert_eq!(netmask_to_prefix("255.0.0.0").unwrap(), 8);
        assert_eq!(netmask_to_prefix("255.255.255.255").unwrap(), 32);
        assert_eq!(netmask_to_prefix("0.0.0.0").unwrap(), 0);
    }

    #[test]
    fn test_netmask_to_prefix_invalid() {
        // Invalid netmask: has zeros before ones
        assert!(netmask_to_prefix("255.255.1.0").is_err());
        assert!(netmask_to_prefix("255.0.255.0").is_err());
    }

    #[test]
    fn test_calculate_network_cidr() {
        // Test case from the spec: 192.168.129.200 with 255.255.248.0
        let result = calculate_network_cidr("192.168.129.200", "255.255.248.0").unwrap();
        assert_eq!(result, "192.168.128.0/21");

        // Test /24 network
        let result = calculate_network_cidr("10.0.1.50", "255.255.255.0").unwrap();
        assert_eq!(result, "10.0.1.0/24");

        // Test /16 network
        let result = calculate_network_cidr("172.16.45.67", "255.255.0.0").unwrap();
        assert_eq!(result, "172.16.0.0/16");
    }

    #[test]
    fn test_generate_network_patch_worker() {
        let nics = vec![Nic {
            mac: "90:b8:d0:2f:1a:62".to_string(),
            primary: true,
            ip: "192.168.129.200".to_string(),
            netmask: "255.255.248.0".to_string(),
            gateway: Some("192.168.128.1".to_string()),
            network: "12345678-1234-1234-1234-123456789012".parse().unwrap(),
            state: None,
        }];

        let nameservers = vec!["8.8.8.8".to_string()];

        let yaml = generate_network_patch(&nics, &nameservers, false, None).unwrap();

        // Verify YAML structure - use more flexible matching
        assert!(yaml.contains("machine:"));
        assert!(yaml.contains("kubelet:"));
        assert!(yaml.contains("nodeIP:"));
        assert!(yaml.contains("validSubnets:"));
        assert!(yaml.contains("192.168.128.0/21"));
        assert!(yaml.contains("network:"));
        assert!(yaml.contains("nameservers:"));
        assert!(yaml.contains("8.8.8.8"));
        assert!(yaml.contains("interfaces:"));
        assert!(yaml.contains("deviceSelector:"));
        assert!(yaml.contains("hardwareAddr:") && yaml.contains("90:b8:d0:2f:1a:62"));
        assert!(yaml.contains("addresses:"));
        assert!(yaml.contains("192.168.129.200/21"));
        assert!(yaml.contains("routes:"));
        assert!(yaml.contains("0.0.0.0/0"));
        assert!(yaml.contains("192.168.128.1"));

        // Should not have cluster config for worker
        assert!(!yaml.contains("cluster:"));
        assert!(!yaml.contains("etcd:"));
    }

    #[test]
    fn test_generate_network_patch_control_plane() {
        let nics = vec![Nic {
            mac: "90:b8:d0:2f:1a:62".to_string(),
            primary: true,
            ip: "192.168.129.200".to_string(),
            netmask: "255.255.248.0".to_string(),
            gateway: Some("192.168.128.1".to_string()),
            network: "12345678-1234-1234-1234-123456789012".parse().unwrap(),
            state: None,
        }];

        let nameservers = vec!["8.8.8.8".to_string()];

        let yaml = generate_network_patch(&nics, &nameservers, true, None).unwrap();

        // Should have cluster config for control plane
        assert!(yaml.contains("cluster:"));
        assert!(yaml.contains("etcd:"));
        assert!(yaml.contains("advertisedSubnets:"));
        assert!(yaml.contains("192.168.128.0/21"));
    }

    #[test]
    fn test_generate_network_patch_multiple_nics() {
        let nics = vec![
            Nic {
                mac: "90:b8:d0:2f:1a:62".to_string(),
                primary: true,
                ip: "192.168.129.200".to_string(),
                netmask: "255.255.248.0".to_string(),
                gateway: Some("192.168.128.1".to_string()),
                network: "12345678-1234-1234-1234-123456789012".parse().unwrap(),
                state: None,
            },
            Nic {
                mac: "90:b8:d0:2f:1a:63".to_string(),
                primary: false,
                ip: "10.0.1.50".to_string(),
                netmask: "255.255.255.0".to_string(),
                gateway: Some("10.0.1.1".to_string()),
                network: "87654321-4321-4321-4321-210987654321".parse().unwrap(),
                state: None,
            },
        ];

        let nameservers = vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()];

        let yaml = generate_network_patch(&nics, &nameservers, false, None).unwrap();

        // Should have both NICs - use flexible matching
        assert!(yaml.contains("90:b8:d0:2f:1a:62"));
        assert!(yaml.contains("90:b8:d0:2f:1a:63"));
        assert!(yaml.contains("192.168.129.200/21"));
        assert!(yaml.contains("10.0.1.50/24"));

        // Network CIDR should be based on primary NIC
        assert!(yaml.contains("validSubnets:"));
        assert!(yaml.contains("192.168.128.0/21"));
    }

    #[test]
    fn test_generate_network_patch_no_nics() {
        let nics: Vec<Nic> = vec![];
        let nameservers = vec!["8.8.8.8".to_string()];

        let result = generate_network_patch(&nics, &nameservers, false, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no NICs"));
    }

    #[test]
    fn test_generate_network_patch_no_primary() {
        let nics = vec![Nic {
            mac: "90:b8:d0:2f:1a:62".to_string(),
            primary: false,
            ip: "192.168.129.200".to_string(),
            netmask: "255.255.248.0".to_string(),
            gateway: Some("192.168.128.1".to_string()),
            network: "12345678-1234-1234-1234-123456789012".parse().unwrap(),
            state: None,
        }];

        let nameservers = vec!["8.8.8.8".to_string()];

        let result = generate_network_patch(&nics, &nameservers, false, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no primary NIC found")
        );
    }

    #[test]
    fn test_generate_network_patch_no_gateway() {
        let nics = vec![Nic {
            mac: "90:b8:d0:2f:1a:62".to_string(),
            primary: true,
            ip: "192.168.129.200".to_string(),
            netmask: "255.255.248.0".to_string(),
            gateway: None,
            network: "12345678-1234-1234-1234-123456789012".parse().unwrap(),
            state: None,
        }];

        let nameservers = vec!["8.8.8.8".to_string()];

        let result = generate_network_patch(&nics, &nameservers, false, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing gateway"));
    }

    #[test]
    fn test_generate_network_patch_dual_homed_control_plane() {
        // Simulates a control plane with:
        // - Primary NIC on external network (172.16.x.x) for management access
        // - Secondary NIC on fabric network (192.168.x.x) shared with workers
        let external_network_id: uuid::Uuid =
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".parse().unwrap();
        let fabric_network_id: uuid::Uuid = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".parse().unwrap();

        let nics = vec![
            Nic {
                mac: "90:b8:d0:f2:49:d4".to_string(),
                primary: true,
                ip: "172.16.27.229".to_string(),
                netmask: "255.255.248.0".to_string(),
                gateway: Some("172.16.26.1".to_string()),
                network: external_network_id,
                state: None,
            },
            Nic {
                mac: "90:b8:d0:1f:57:0d".to_string(),
                primary: false,
                ip: "192.168.129.239".to_string(),
                netmask: "255.255.252.0".to_string(),
                gateway: Some("192.168.128.1".to_string()),
                network: fabric_network_id,
                state: None,
            },
        ];

        let nameservers = vec!["8.8.8.8".to_string()];

        // When fabric_network_id is specified, nodeIP and etcd should use fabric subnet
        let yaml =
            generate_network_patch(&nics, &nameservers, true, Some(fabric_network_id)).unwrap();

        // validSubnets should be the fabric network (192.168.128.0/22), NOT external (172.16.24.0/21)
        assert!(yaml.contains("validSubnets:"));
        assert!(yaml.contains("192.168.128.0/22"));
        assert!(!yaml.contains("172.16.24.0/21"));

        // advertisedSubnets should also be the fabric network
        assert!(yaml.contains("advertisedSubnets:"));

        // Both interfaces should still be configured
        assert!(yaml.contains("90:b8:d0:f2:49:d4"));
        assert!(yaml.contains("90:b8:d0:1f:57:0d"));
        assert!(yaml.contains("172.16.27.229/21"));
        assert!(yaml.contains("192.168.129.239/22"));
    }

    #[test]
    fn test_generate_network_patch_dual_homed_no_fabric_specified() {
        // Same dual-homed setup but without specifying fabric_network_id
        // Should fall back to primary NIC (external network)
        let external_network_id: uuid::Uuid =
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".parse().unwrap();
        let fabric_network_id: uuid::Uuid = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".parse().unwrap();

        let nics = vec![
            Nic {
                mac: "90:b8:d0:f2:49:d4".to_string(),
                primary: true,
                ip: "172.16.27.229".to_string(),
                netmask: "255.255.248.0".to_string(),
                gateway: Some("172.16.26.1".to_string()),
                network: external_network_id,
                state: None,
            },
            Nic {
                mac: "90:b8:d0:1f:57:0d".to_string(),
                primary: false,
                ip: "192.168.129.239".to_string(),
                netmask: "255.255.252.0".to_string(),
                gateway: Some("192.168.128.1".to_string()),
                network: fabric_network_id,
                state: None,
            },
        ];

        let nameservers = vec!["8.8.8.8".to_string()];

        // When fabric_network_id is None, falls back to primary NIC
        let yaml = generate_network_patch(&nics, &nameservers, true, None).unwrap();

        // validSubnets should be the external network (fallback to primary)
        assert!(yaml.contains("172.16.24.0/21"));
        assert!(!yaml.contains("192.168.128.0/22"));
    }
}
