// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_update_nics` — DC-wide NIC reassignment sweep.
//!
//! When an operator changes a network's gateway, resolvers, or routes,
//! CNAPI fans the change out to every CN that might host a VM on that
//! network. Each CN runs this task with:
//!
//! * `original_network` — the network's state before the change.
//! * `networks` — the new set of networks (one or more — the legacy
//!   task accepts a list so a single pass can update related subnets).
//!
//! For every local VM with a NIC whose netmask/VLAN/IP-range matches
//! the original network, we compute an update payload (new gateway,
//! resolvers, added/removed routes) and apply it via `vmadm update`.
//!
//! The response is `{vm: <loaded machine>}` for the VM identified by
//! `params.uuid`, matching the legacy shape.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{LoadOptions, LookupOptions, VmadmTool};

#[derive(Debug, Deserialize)]
struct Params {
    /// UUID of the VM to reload after updates. Legacy only uses this
    /// for the response; the sweep itself runs over every matching VM.
    uuid: String,
    /// New network state (may be multiple related subnets).
    networks: Vec<Network>,
    /// Pre-change state of the network whose change triggered this task.
    original_network: Network,
}

#[derive(Debug, Clone, Deserialize)]
struct Network {
    #[serde(default)]
    uuid: Option<String>,
    subnet: String,
    netmask: String,
    vlan_id: u16,
    nic_tag: String,
    #[serde(default)]
    gateway: Option<String>,
    #[serde(default)]
    resolvers: Option<Vec<String>>,
    #[serde(default)]
    routes: Option<BTreeMap<String, String>>,
}

/// Parsed form of `Network.subnet` ("10.0.0.0/24" → start/end integers).
#[derive(Debug, Clone, Copy)]
struct SubnetRange {
    start: u32,
    end: u32,
}

impl Network {
    fn subnet_range(&self) -> Option<SubnetRange> {
        let (addr, prefix) = self.subnet.split_once('/')?;
        let start: u32 = addr.parse::<Ipv4Addr>().ok()?.into();
        let prefix: u32 = prefix.parse().ok()?;
        if prefix > 32 {
            return None;
        }
        let size = if prefix == 0 {
            u64::from(u32::MAX) + 1
        } else {
            1u64 << (32 - prefix)
        };
        let end = start.saturating_add((size - 1) as u32);
        Some(SubnetRange { start, end })
    }
}

/// Returns true if the nic's parameters indicate that it is on the network.
fn network_matches_nic(network: &Network, nic: &serde_json::Value) -> bool {
    let Some(range) = network.subnet_range() else {
        return false;
    };
    let nic_vlan = nic.get("vlan_id").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    if nic_vlan != network.vlan_id {
        return false;
    }
    let nic_netmask = nic.get("netmask").and_then(|v| v.as_str()).unwrap_or("");
    if nic_netmask != network.netmask {
        return false;
    }
    let Some(ip_str) = nic.get("ip").and_then(|v| v.as_str()) else {
        return false;
    };
    let Ok(ip) = ip_str.parse::<Ipv4Addr>() else {
        return false;
    };
    let ip: u32 = ip.into();
    // Legacy uses `start <= ipNum && ipNum < end` (half-open); preserve.
    ip >= range.start && ip < range.end
}

/// Build an update payload for a single VM based on the `networks`
/// supplied in this task. Returns `None` if the VM has no NICs on the
/// original network or if nothing actually needs to change.
fn build_update_for_vm(
    vm: &serde_json::Value,
    original: &Network,
    networks: &[Network],
) -> Option<VmUpdate> {
    let nics = vm.get("nics")?.as_array()?;

    let mut matched = false;
    let mut update_nics: Vec<serde_json::Value> = Vec::new();
    let mut resolvers: Vec<String> = Vec::new();
    let mut new_routes: BTreeMap<String, String> = BTreeMap::new();

    let set_resolvers = vm
        .pointer("/internal_metadata/set_resolvers")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    for nic in nics {
        let is_matching_original = network_matches_nic(original, nic);
        if is_matching_original {
            matched = true;
        }

        for network in networks {
            if !network_matches_nic(network, nic) {
                continue;
            }

            if is_matching_original && let Some(gateway) = &network.gateway {
                let current_gateway = nic.get("gateway").and_then(|v| v.as_str()).unwrap_or("");
                if current_gateway != gateway {
                    let mac = nic.get("mac").and_then(|v| v.as_str()).unwrap_or("");
                    update_nics.push(serde_json::json!({
                        "gateway": gateway,
                        "mac": mac,
                    }));
                }
            }

            if let Some(rs) = &network.resolvers {
                for r in rs {
                    if !resolvers.contains(r) {
                        resolvers.push(r.clone());
                    }
                }
            }

            if let Some(routes) = &network.routes {
                for (k, v) in routes {
                    new_routes.insert(k.clone(), v.clone());
                }
            }
        }
    }

    if !matched {
        return None;
    }

    let old_routes: BTreeMap<String, String> = vm
        .get("routes")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let (set_routes, remove_routes) = diff_routes(&old_routes, &new_routes);

    let mut update = VmUpdate::default();
    if set_resolvers {
        update.resolvers = Some(resolvers);
    }
    if !update_nics.is_empty() {
        update.update_nics = Some(update_nics);
    }
    if !set_routes.is_empty() {
        update.set_routes = Some(set_routes);
    }
    if !remove_routes.is_empty() {
        update.remove_routes = Some(remove_routes);
    }

    if update.is_empty() {
        None
    } else {
        Some(update)
    }
}

/// Compute `(set, remove)` routes between old and new. Reproduces
/// `add_route_properties` from the legacy task:
/// * Only in new → set
/// * Only in old → remove
/// * In both but unchanged → neither
/// * In both with changed value → set (will overwrite)
fn diff_routes(
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, Vec<String>) {
    if old.is_empty() && new.is_empty() {
        return (BTreeMap::new(), Vec::new());
    }
    if old.is_empty() {
        return (new.clone(), Vec::new());
    }
    if new.is_empty() {
        return (BTreeMap::new(), old.keys().cloned().collect());
    }

    let mut set = BTreeMap::new();
    let mut remove_keys: BTreeSet<String> = old.keys().cloned().collect();
    for (k, v) in new {
        if let Some(old_v) = old.get(k) {
            remove_keys.remove(k);
            if old_v != v {
                set.insert(k.clone(), v.clone());
            }
        } else {
            set.insert(k.clone(), v.clone());
        }
    }
    (set, remove_keys.into_iter().collect())
}

/// Accumulated vmadm update payload.
#[derive(Debug, Default, Clone)]
struct VmUpdate {
    resolvers: Option<Vec<String>>,
    update_nics: Option<Vec<serde_json::Value>>,
    set_routes: Option<BTreeMap<String, String>>,
    remove_routes: Option<Vec<String>>,
}

impl VmUpdate {
    fn is_empty(&self) -> bool {
        self.resolvers.is_none()
            && self.update_nics.is_none()
            && self.set_routes.is_none()
            && self.remove_routes.is_none()
    }

    fn into_payload(self, uuid: &str) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "uuid".to_string(),
            serde_json::Value::String(uuid.to_string()),
        );
        if let Some(r) = self.resolvers {
            obj.insert("resolvers".to_string(), serde_json::json!(r));
        }
        if let Some(n) = self.update_nics {
            obj.insert("update_nics".to_string(), serde_json::Value::Array(n));
        }
        if let Some(s) = self.set_routes {
            obj.insert("set_routes".to_string(), serde_json::json!(s));
        }
        if let Some(r) = self.remove_routes {
            obj.insert("remove_routes".to_string(), serde_json::json!(r));
        }
        serde_json::Value::Object(obj)
    }
}

pub struct MachineUpdateNicsTask {
    tool: Arc<VmadmTool>,
}

impl MachineUpdateNicsTask {
    pub fn new(tool: Arc<VmadmTool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl TaskHandler for MachineUpdateNicsTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        // pre_check: networks non-empty + every subnet parses, original
        // subnet parses. Matches the legacy invalid-param messages.
        let mut invalid: Vec<String> = Vec::new();
        if p.networks.is_empty() {
            invalid.push("networks".to_string());
        } else {
            for n in &p.networks {
                if n.subnet_range().is_none() {
                    let id = n.uuid.as_deref().unwrap_or("<unknown>");
                    invalid.push(format!("networks ({id})"));
                }
            }
        }
        if p.original_network.subnet_range().is_none() {
            invalid.push("original_network".to_string());
        }
        if !invalid.is_empty() {
            let plural = if invalid.len() == 1 { "" } else { "s" };
            return Err(TaskError::new(format!(
                "Invalid request parameter{plural}: {}",
                invalid.join(", ")
            )));
        }

        // Lookup VMs on the original network via vmadm filter. We can't
        // filter by IP range directly; match the legacy "nic_tag + netmask
        // + vlan_id" lookup, then do the IP-range filter locally.
        let mut search = BTreeMap::new();
        search.insert(
            "nics.*.nic_tag".to_string(),
            p.original_network.nic_tag.clone(),
        );
        search.insert(
            "nics.*.netmask".to_string(),
            p.original_network.netmask.clone(),
        );
        search.insert(
            "nics.*.vlan_id".to_string(),
            p.original_network.vlan_id.to_string(),
        );
        let lookup_opts = LookupOptions {
            include_dni: false,
            fields: Some(
                ["uuid", "nics", "internal_metadata", "routes"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        };
        let vms = self
            .tool
            .lookup(&search, &lookup_opts)
            .await
            .map_err(|e| vmadm_error_to_task(e, "Error looking up VMs"))?;

        let mut updates: Vec<(String, VmUpdate)> = Vec::new();
        for vm in vms {
            let Some(uuid) = vm.get("uuid").and_then(|v| v.as_str()).map(str::to_string) else {
                continue;
            };
            if let Some(update) = build_update_for_vm(&vm, &p.original_network, &p.networks) {
                updates.push((uuid, update));
            }
        }

        tracing::info!(count = updates.len(), "VMs to update");

        for (uuid, update) in updates {
            let payload = update.into_payload(&uuid);
            self.tool
                .update(&uuid, &payload, false)
                .await
                .map_err(|e| vmadm_error_to_task(e, &format!(r#"Error updating VM "{uuid}""#)))?;
        }

        // Return the load of `params.uuid` to match legacy shape.
        let load_opts = LoadOptions {
            include_dni: false,
            fields: None,
        };
        let vm = self
            .tool
            .load(&p.uuid, &load_opts)
            .await
            .map_err(|e| vmadm_error_to_task(e, "vmadm.load error"))?;
        Ok(serde_json::json!({ "vm": vm }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(subnet: &str, netmask: &str, vlan: u16, gateway: Option<&str>) -> Network {
        Network {
            uuid: None,
            subnet: subnet.to_string(),
            netmask: netmask.to_string(),
            vlan_id: vlan,
            nic_tag: "external".to_string(),
            gateway: gateway.map(str::to_string),
            resolvers: None,
            routes: None,
        }
    }

    #[test]
    fn subnet_range_parses_known_prefixes() {
        let r = net("10.0.0.0/24", "255.255.255.0", 0, None)
            .subnet_range()
            .expect("range");
        assert_eq!(r.start, 0x0a000000);
        assert_eq!(r.end, 0x0a0000ff);
    }

    #[test]
    fn network_matches_nic_checks_vlan_netmask_and_range() {
        let network = net("10.0.0.0/24", "255.255.255.0", 101, None);
        let nic = serde_json::json!({
            "ip": "10.0.0.5",
            "netmask": "255.255.255.0",
            "vlan_id": 101
        });
        assert!(network_matches_nic(&network, &nic));

        let wrong_vlan = serde_json::json!({
            "ip": "10.0.0.5",
            "netmask": "255.255.255.0",
            "vlan_id": 202
        });
        assert!(!network_matches_nic(&network, &wrong_vlan));

        let out_of_range = serde_json::json!({
            "ip": "10.0.1.5",
            "netmask": "255.255.255.0",
            "vlan_id": 101
        });
        assert!(!network_matches_nic(&network, &out_of_range));
    }

    #[test]
    fn diff_routes_handles_all_four_cases() {
        // both empty → nothing to do
        let (s, r) = diff_routes(&BTreeMap::new(), &BTreeMap::new());
        assert!(s.is_empty() && r.is_empty());

        // only new → all set
        let mut new = BTreeMap::new();
        new.insert("1.0.0.0/8".into(), "10.0.0.1".into());
        let (s, r) = diff_routes(&BTreeMap::new(), &new);
        assert_eq!(s.len(), 1);
        assert!(r.is_empty());

        // only old → all remove
        let mut old = BTreeMap::new();
        old.insert("1.0.0.0/8".into(), "10.0.0.1".into());
        let (s, r) = diff_routes(&old, &BTreeMap::new());
        assert!(s.is_empty());
        assert_eq!(r, vec!["1.0.0.0/8".to_string()]);

        // old value changed → set new, don't remove
        let mut old = BTreeMap::new();
        old.insert("1.0.0.0/8".into(), "10.0.0.1".into());
        let mut new = BTreeMap::new();
        new.insert("1.0.0.0/8".into(), "10.0.0.99".into());
        let (s, r) = diff_routes(&old, &new);
        assert_eq!(s.get("1.0.0.0/8").unwrap(), "10.0.0.99");
        assert!(r.is_empty());
    }

    #[test]
    fn build_update_picks_gateway_only_for_matching_network() {
        let original = net("10.0.0.0/24", "255.255.255.0", 0, Some("10.0.0.1"));
        let new = net("10.0.0.0/24", "255.255.255.0", 0, Some("10.0.0.254"));
        let vm = serde_json::json!({
            "uuid": "abc",
            "nics": [
                {
                    "ip": "10.0.0.5",
                    "netmask": "255.255.255.0",
                    "vlan_id": 0,
                    "gateway": "10.0.0.1",
                    "mac": "aa:bb:cc:dd:ee:01"
                }
            ]
        });
        let update = build_update_for_vm(&vm, &original, &[new]).expect("update");
        let nics = update.update_nics.expect("update_nics");
        assert_eq!(nics.len(), 1);
        assert_eq!(nics[0]["gateway"], "10.0.0.254");
        assert_eq!(nics[0]["mac"], "aa:bb:cc:dd:ee:01");
    }

    #[test]
    fn build_update_returns_none_when_no_nic_matches_original() {
        let original = net("10.0.0.0/24", "255.255.255.0", 0, Some("10.0.0.1"));
        let vm = serde_json::json!({
            "uuid": "abc",
            "nics": [{
                "ip": "192.168.1.1",
                "netmask": "255.255.255.0",
                "vlan_id": 0
            }]
        });
        assert!(build_update_for_vm(&vm, &original, std::slice::from_ref(&original)).is_none());
    }
}
