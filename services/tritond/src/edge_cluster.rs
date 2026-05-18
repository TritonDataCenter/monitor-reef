// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! M1 edge-cluster materialisation for NAT gateways: CN selection,
//! apply-job queueing, fhrun manifest placement, and the underlay-IPv6
//! discovery heuristics.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

use dropshot::HttpError;
use uuid::Uuid;

use tritond_api::types::{JobKind, JobStatus, NatGateway, Route, RouteTarget};
use tritond_store::{
    Cn, CnRole, CnState, EdgeCluster, EdgeClusterInstance, EdgeClusterInstanceState,
    EdgeClusterKind, EdgeClusterResource, EdgeNicCoord, NewEdgeCluster, NewJob, Store, StoreError,
};

use crate::edge;
use crate::error::store_error_to_http;

const M1_MAX_EDGE_INSTANCES_PER_CN: usize = 2;
const EDGE_ROOT: &str = "/var/lib/tritonagent/edge";
const EDGE_FIREHYVE_BIN: &str = "/opt/firehyve/bin/firehyve";
const EDGE_KERNEL: &str = "/opt/firehyve/kernels/linux-v1/bzImage";
const EDGE_INIT: &str = "/opt/firehyve/bin/fhrun-init";
const EDGE_AGENT_BIN: &str = "/opt/firehyve/bin/edge-agent";
const EDGE_VM_MEMORY: &str = "128M";

pub(crate) async fn ensure_nat_gateway_edges_for_routes(
    store: &dyn Store,
    routes: &[Route],
) -> Result<(), HttpError> {
    let mut nat_gateway_ids = Vec::new();
    for route in routes {
        if let RouteTarget::NatGateway { nat_gateway_id } = route.target
            && !nat_gateway_ids.contains(&nat_gateway_id)
        {
            nat_gateway_ids.push(nat_gateway_id);
        }
    }

    for nat_gateway_id in nat_gateway_ids {
        ensure_nat_gateway_edge_materialized(store, nat_gateway_id).await?;
    }
    Ok(())
}

pub(crate) async fn ensure_nat_gateway_edge_materialized(
    store: &dyn Store,
    nat_gateway_id: Uuid,
) -> Result<(), HttpError> {
    let nat_gateway = store
        .get_nat_gateway(nat_gateway_id)
        .await
        .map_err(store_error_to_http)?;
    if let Some(edge_cluster_id) = nat_gateway.edge_cluster_id {
        let cluster = store
            .get_edge_cluster(edge_cluster_id)
            .await
            .map_err(store_error_to_http)?;
        ensure_edge_apply_job_for_nat_gateway(store, &nat_gateway, &cluster).await?;
        return Ok(());
    }

    let bound_resource = EdgeClusterResource::NatGateway { nat_gateway_id };
    let existing = store
        .list_edge_clusters_for_resource(bound_resource)
        .await
        .map_err(store_error_to_http)?;
    if let Some(cluster) = existing.first() {
        ensure_edge_apply_job_for_nat_gateway(store, &nat_gateway, cluster).await?;
        return Ok(());
    }

    let (edge_cn, underlay) = select_edge_cn_for_nat_gateway(store).await?;
    let edge_instance = new_m1_edge_instance(&nat_gateway, edge_cn.server_uuid, underlay);
    let cluster = store
        .create_edge_cluster(NewEdgeCluster {
            name: edge_cluster_name(nat_gateway_id),
            kind: EdgeClusterKind::NatGateway,
            bound_resources: vec![bound_resource],
            instances: vec![edge_instance.clone()],
        })
        .await
        .map_err(store_error_to_http)?;
    let nat_gateway = store
        .get_nat_gateway(nat_gateway_id)
        .await
        .map_err(store_error_to_http)?;
    ensure_edge_apply_job_for_nat_gateway(store, &nat_gateway, &cluster).await?;

    tracing::info!(
        nat_gateway_id = %nat_gateway.id,
        edge_cluster_id = %cluster.id,
        edge_instance_id = %edge_instance.id,
        target_cn_uuid = %edge_cn.server_uuid,
        "materialized M1 NAT edge cluster"
    );
    Ok(())
}

pub(crate) async fn ensure_edge_apply_job_for_nat_gateway(
    store: &dyn Store,
    nat_gateway: &NatGateway,
    cluster: &EdgeCluster,
) -> Result<(), HttpError> {
    if cluster
        .realized
        .applied_generation
        .is_some_and(|generation| generation >= cluster.desired_generation)
    {
        return Ok(());
    }
    if edge_apply_job_in_flight(store, cluster).await? {
        return Ok(());
    }

    let edge_instance = cluster.instances.first().ok_or_else(|| {
        store_error_to_http(StoreError::Conflict(format!(
            "edge cluster {} has no instances to apply",
            cluster.id
        )))
    })?;
    let bindings = edge_manifest_bindings_for_nat_gateway(store, nat_gateway).await?;
    let manifest = edge::render_edge_manifest(
        nat_gateway,
        &bindings,
        &edge_manifest_placement(edge_instance).map_err(store_error_to_http)?,
    );
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| HttpError::for_internal_error(format!("serialize edge manifest: {err}")))?;

    store
        .enqueue_job(NewJob {
            kind: JobKind::EdgeApply {
                edge_cluster_id: cluster.id,
                edge_instance_id: edge_instance.id,
                desired_generation: cluster.desired_generation,
                manifest_bytes,
            },
            target_cn_uuid: Some(edge_instance.cn_id),
        })
        .await
        .map_err(store_error_to_http)?;

    tracing::info!(
        nat_gateway_id = %nat_gateway.id,
        edge_cluster_id = %cluster.id,
        edge_instance_id = %edge_instance.id,
        target_cn_uuid = %edge_instance.cn_id,
        desired_generation = cluster.desired_generation,
        "queued M1 NAT edge apply"
    );
    Ok(())
}

pub(crate) async fn edge_apply_job_in_flight(
    store: &dyn Store,
    cluster: &EdgeCluster,
) -> Result<bool, HttpError> {
    let jobs = store
        .list_recent_jobs(1024)
        .await
        .map_err(store_error_to_http)?;
    Ok(jobs.iter().any(|job| {
        matches!(job.status, JobStatus::Pending | JobStatus::InProgress)
            && matches!(
                &job.kind,
                JobKind::EdgeApply {
                    edge_cluster_id,
                    desired_generation,
                    ..
                } if *edge_cluster_id == cluster.id
                    && *desired_generation >= cluster.desired_generation
            )
    }))
}

pub(crate) async fn edge_clusters_for_nat_gateways(
    store: &dyn Store,
    nat_gateways: &[NatGateway],
) -> Result<Vec<EdgeCluster>, HttpError> {
    let mut cluster_ids = Vec::new();
    for nat in nat_gateways {
        if let Some(edge_cluster_id) = nat.edge_cluster_id
            && !cluster_ids.contains(&edge_cluster_id)
        {
            cluster_ids.push(edge_cluster_id);
        }
    }

    let mut out = Vec::with_capacity(cluster_ids.len());
    for id in cluster_ids {
        out.push(
            store
                .get_edge_cluster(id)
                .await
                .map_err(store_error_to_http)?,
        );
    }
    Ok(out)
}

pub(crate) async fn edge_manifest_bindings_for_nat_gateway(
    store: &dyn Store,
    nat_gateway: &NatGateway,
) -> Result<edge::EdgeManifestBindings, HttpError> {
    let subnets = store
        .list_subnets_in_vpc(nat_gateway.vpc_id)
        .await
        .map_err(store_error_to_http)?;
    let mut snat_sources = Vec::new();

    for subnet in subnets {
        let routes = store
            .list_routes_in_table(subnet.route_table_id)
            .await
            .map_err(store_error_to_http)?;
        for route in routes {
            if !matches!(
                route.target,
                RouteTarget::NatGateway { nat_gateway_id } if nat_gateway_id == nat_gateway.id
            ) {
                continue;
            }
            match route.destination.ip() {
                IpAddr::V4(_) => {
                    if let Some(cidr) = subnet.ipv4_block {
                        snat_sources.push(cidr.to_string());
                    }
                }
                IpAddr::V6(_) => {
                    if let Some(cidr) = subnet.ipv6_block {
                        snat_sources.push(cidr.to_string());
                    }
                }
            }
        }
    }
    snat_sources.sort();
    snat_sources.dedup();

    Ok(edge::EdgeManifestBindings {
        snat_sources,
        floating_ips: Vec::new(),
    })
}

pub(crate) async fn select_edge_cn_for_nat_gateway(
    store: &dyn Store,
) -> Result<(Cn, Ipv6Addr), HttpError> {
    let cns = store
        .list_cns(Some(CnState::Approved))
        .await
        .map_err(store_error_to_http)?;
    let edge_counts = edge_instance_counts_by_cn(store).await?;
    let mut best: Option<(usize, u128, Cn, Ipv6Addr)> = None;

    for cn in cns
        .into_iter()
        .filter(|cn| cn_accepts_edge_jobs(cn))
        .filter_map(|cn| edge_cn_underlay_ipv6(&cn).map(|underlay| (cn, underlay)))
    {
        let assigned = edge_counts.get(&cn.0.server_uuid).copied().unwrap_or(0);
        if assigned >= M1_MAX_EDGE_INSTANCES_PER_CN {
            continue;
        }
        let key = (assigned, cn.0.server_uuid.as_u128(), cn.0, cn.1);
        if best
            .as_ref()
            .is_none_or(|current| (key.0, key.1) < (current.0, current.1))
        {
            best = Some(key);
        }
    }

    best.map(|(_, _, cn, underlay)| (cn, underlay))
        .ok_or_else(|| {
            store_error_to_http(StoreError::Conflict(
                "no eligible edge CN with IPv6 underlay available for NAT gateway placement"
                    .to_string(),
            ))
        })
}

pub(crate) async fn edge_instance_counts_by_cn(
    store: &dyn Store,
) -> Result<std::collections::HashMap<Uuid, usize>, HttpError> {
    let clusters = store
        .list_edge_clusters()
        .await
        .map_err(store_error_to_http)?;
    let mut counts = std::collections::HashMap::new();
    for instance in clusters.iter().flat_map(|cluster| cluster.instances.iter()) {
        *counts.entry(instance.cn_id).or_insert(0) += 1;
    }
    Ok(counts)
}

pub(crate) fn cn_accepts_edge_jobs(cn: &Cn) -> bool {
    cn.state == CnState::Approved
        && cn.last_seen.is_some()
        && matches!(cn.role, CnRole::Edge | CnRole::Both)
}

pub(crate) fn edge_cn_underlay_ipv6(cn: &Cn) -> Option<Ipv6Addr> {
    let key_paths = [
        "triton_edge_underlay_ipv6",
        "triton_edge_underlay",
        "proteus_underlay_ipv6",
        "underlay_ipv6",
        "edge_underlay_ipv6",
    ];
    for key in key_paths {
        if let Some(addr) = cn
            .sysinfo
            .get(key)
            .and_then(first_ipv6_from_value)
            .or_else(|| {
                cn.last_status
                    .as_ref()
                    .and_then(|status| status.get(key))
                    .and_then(first_ipv6_from_value)
            })
        {
            return Some(addr);
        }
    }

    cn.sysinfo
        .get("Network Interfaces")
        .and_then(first_interface_ipv6)
        .or_else(|| {
            cn.last_status
                .as_ref()
                .and_then(|status| status.get("Network Interfaces"))
                .and_then(first_interface_ipv6)
        })
        .or_else(|| cn.admin_ip.and_then(lab_underlay_from_admin_ipv4))
        .or_else(|| {
            cn.sysinfo
                .get("Admin IP")
                .and_then(first_ipv4_from_value)
                .and_then(lab_underlay_from_admin_ipv4)
        })
}

pub(crate) fn first_interface_ipv6(value: &serde_json::Value) -> Option<Ipv6Addr> {
    let interfaces = value.as_object()?;
    for iface in interfaces.values() {
        if let Some(addr) = ["ip6addr", "ip6addr0", "IPv6 Address", "ipv6"]
            .iter()
            .find_map(|key| iface.get(*key).and_then(first_ipv6_from_value))
        {
            return Some(addr);
        }
    }
    None
}

pub(crate) fn first_ipv6_from_value(value: &serde_json::Value) -> Option<Ipv6Addr> {
    match value {
        serde_json::Value::String(s) => parse_ipv6_hint(s),
        serde_json::Value::Array(values) => values.iter().find_map(first_ipv6_from_value),
        _ => None,
    }
}

pub(crate) fn first_ipv4_from_value(value: &serde_json::Value) -> Option<Ipv4Addr> {
    match value {
        serde_json::Value::String(s) => s.parse().ok(),
        serde_json::Value::Array(values) => values.iter().find_map(first_ipv4_from_value),
        _ => None,
    }
}

pub(crate) fn parse_ipv6_hint(value: &str) -> Option<Ipv6Addr> {
    let without_prefix = value.split('/').next().unwrap_or(value);
    let without_zone = without_prefix.split('%').next().unwrap_or(without_prefix);
    let addr = without_zone.parse::<Ipv6Addr>().ok()?;
    if addr.is_unspecified() || addr.is_loopback() || addr.is_multicast() {
        return None;
    }
    Some(addr)
}

pub(crate) fn lab_underlay_from_admin_ipv4(addr: Ipv4Addr) -> Option<Ipv6Addr> {
    // M1 lab convention: nuc admin IPv4 10.199.199.X maps to fd00::X.
    if addr.octets()[0..3] != [10, 199, 199] {
        return None;
    }
    format!("fd00::{}", addr.octets()[3]).parse().ok()
}

#[cfg(test)]
mod edge_underlay_tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn edge_cn(admin_ip: Option<Ipv4Addr>, sysinfo: serde_json::Value) -> Cn {
        let now = Utc::now();
        Cn {
            server_uuid: Uuid::new_v4(),
            hostname: "edge-a".to_string(),
            admin_ip,
            state: CnState::Approved,
            role: CnRole::Edge,
            registered_at: now,
            approved_at: Some(now),
            last_seen: Some(now),
            sysinfo,
            claim_code: None,
            claim_code_expires_at: None,
            poll_token: "poll".to_string(),
            bound_api_key_id: None,
            pending_credential: None,
            last_status: None,
            console_listen_port: None,
            console_tls_spki_sha256: None,
            console_ticket_key: None,
            imds_token_key: None,
        }
    }

    #[test]
    fn edge_underlay_prefers_explicit_ipv6_hint() {
        let cn = edge_cn(
            Some("10.199.199.40".parse().unwrap()),
            json!({ "triton_edge_underlay_ipv6": "fd00::99" }),
        );

        assert_eq!(
            edge_cn_underlay_ipv6(&cn),
            Some("fd00::99".parse().unwrap())
        );
    }

    #[test]
    fn edge_underlay_falls_back_to_m1_lab_admin_ipv4() {
        let cn = edge_cn(Some("10.199.199.40".parse().unwrap()), json!({}));

        assert_eq!(
            edge_cn_underlay_ipv6(&cn),
            Some("fd00::40".parse().unwrap())
        );
    }

    #[test]
    fn edge_underlay_ignores_non_lab_admin_ipv4() {
        let cn = edge_cn(Some("192.0.2.40".parse().unwrap()), json!({}));

        assert_eq!(edge_cn_underlay_ipv6(&cn), None);
    }
}

pub(crate) fn new_m1_edge_instance(
    nat_gateway: &NatGateway,
    cn_id: Uuid,
    underlay: Ipv6Addr,
) -> EdgeClusterInstance {
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    EdgeClusterInstance {
        id,
        cn_id,
        fhrun_manifest_uri: format!("{EDGE_ROOT}/{id}/manifest.json"),
        north_nic: EdgeNicCoord {
            nic_tag: edge_vnic_name(id, edge::EDGE_NIC_ROLE_NORTH),
            mac: Some(edge_mac(id, 0x10)),
            ip: Some(nat_gateway.public_address),
        },
        south_nic: EdgeNicCoord {
            nic_tag: edge_vnic_name(id, edge::EDGE_NIC_ROLE_SOUTH),
            mac: Some(edge_mac(id, 0x11)),
            ip: Some(IpAddr::V6(underlay)),
        },
        control_socket: format!("{EDGE_ROOT}/{id}/edge-control.sock"),
        state: EdgeClusterInstanceState::Pending,
        last_error: None,
        created_at: now,
        updated_at: now,
    }
}

pub(crate) fn edge_manifest_placement(
    instance: &EdgeClusterInstance,
) -> Result<edge::EdgeManifestPlacement, StoreError> {
    Ok(edge::EdgeManifestPlacement {
        edge_instance_id: instance.id,
        firehyve: PathBuf::from(EDGE_FIREHYVE_BIN),
        kernel: PathBuf::from(EDGE_KERNEL),
        init: PathBuf::from(EDGE_INIT),
        edge_agent_bin: PathBuf::from(EDGE_AGENT_BIN),
        edge_control_socket: PathBuf::from(&instance.control_socket),
        north_nic: edge_manifest_nic(&instance.north_nic)?,
        south_nic: edge_manifest_nic(&instance.south_nic)?,
        vcpus: 1,
        memory: EDGE_VM_MEMORY.to_string(),
    })
}

pub(crate) fn edge_manifest_nic(nic: &EdgeNicCoord) -> Result<edge::EdgeNicPlacement, StoreError> {
    let mac = nic
        .mac
        .clone()
        .ok_or_else(|| StoreError::Backend("edge instance NIC is missing a MAC".to_string()))?;
    let ip = nic
        .ip
        .ok_or_else(|| StoreError::Backend("edge instance NIC is missing an IP".to_string()))?;
    Ok(edge::EdgeNicPlacement {
        vnic: nic.nic_tag.clone(),
        mac,
        ip: host_cidr(ip),
        gateway: None,
    })
}

pub(crate) fn edge_cluster_name(nat_gateway_id: Uuid) -> String {
    format!("edge-nat-{}", nat_gateway_id.simple())
}

pub(crate) fn edge_vnic_name(edge_instance_id: Uuid, role: &str) -> String {
    let simple = edge_instance_id.simple().to_string();
    format!("edge-{}-{role}", &simple[..8])
}

pub(crate) fn edge_mac(edge_instance_id: Uuid, salt: u8) -> String {
    let bytes = edge_instance_id.as_bytes();
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0] ^ salt,
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4]
    )
}

pub(crate) fn host_cidr(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => format!("{v4}/32"),
        IpAddr::V6(v6) => format!("{v6}/128"),
    }
}
