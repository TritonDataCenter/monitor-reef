// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Agent blueprint materialisation: the provisioning blueprint, the
//! Proteus per-port `PortBlueprint`, and the tritond → triton-vpc
//! intent conversions it folds in.

use base64::Engine;
use dropshot::{ClientErrorStatusCode, HttpError};
use std::net::IpAddr;
use uuid::Uuid;

use proteus_api::blueprint::{
    ClientLinkConfig, PORT_BLUEPRINT_SCHEMA_V0, PluginConfigBytes, PortBlueprint, PortLimits,
};
use proteus_api::ids::{
    Generation as ProteusGeneration, NetworkId as ProteusNetworkId, PortId as ProteusPortId,
};
use triton_vpc::TRITON_VPC_BLUEPRINT_SCHEMA_V1;
use triton_vpc::tritond_intent_v1::{
    EdgeClusterIntentV1, FirewallActionIntentV1, FirewallDirectionIntentV1, FirewallRuleIntentV1,
    FloatingIpAttachmentIntentV1, FloatingIpIntentV1, L4ProtocolIntentV1, NatGatewayIntentV1,
    NicIntentV1, PortRangeIntentV1, RouteIntentV1, RouteTargetIntentV1, SubnetIntentV1,
    TritondPortIntentV1, VpcIntentV1,
};
use tritond_api::types::{
    FloatingIp, Instance, JobKind, JobStatus, ManagedIdentity, NatGateway, ProvisioningJob, Route,
    RouteTarget, Subnet,
};
use tritond_api::{AgentPortBlueprint, ProvisioningBlueprint};
use tritond_store::{EdgeCluster, EdgeClusterInstanceState, Store, StoreError};

use crate::cn_credential::enforce_job_belongs_to_bound_cn;
use crate::edge_cluster::{edge_clusters_for_nat_gateways, ensure_nat_gateway_edges_for_routes};
use crate::error::{not_found, store_error_to_http};

const INITIAL_PROTEUS_PORT_GENERATION: u64 = 1;

/// Materialise the agent-side blueprint for a job. Resolves
/// instance + image + nics + disks + ssh public keys for a
/// `Provision`; returns just the instance (when still extant)
/// for `Stop` / `Restart`.
///
/// Errors from the store path bubble up as HTTP errors via
/// [`store_error_to_http`]. A concurrent operator delete that
/// removes the instance after the job was claimed surfaces as
/// `instance: None` rather than a 404; the agent then reports
/// `JobOutcome::Failed { reason: "instance gone" }`.
pub(crate) async fn build_blueprint(
    store: &dyn Store,
    identity_hmac_key: &tritond_auth::IdentityHmacKey,
    job: &ProvisioningJob,
) -> Result<ProvisioningBlueprint, HttpError> {
    let Some(instance_id) = job.kind.instance_id() else {
        return Ok(ProvisioningBlueprint {
            job_id: job.id,
            kind: job.kind.clone(),
            instance: None,
            image: None,
            nics: Vec::new(),
            subnets: Vec::new(),
            disks: Vec::new(),
            ssh_public_keys: Vec::new(),
            managed_identity: None,
        });
    };
    let instance = match store.get_instance(instance_id).await {
        Ok(i) => Some(i),
        Err(StoreError::NotFound) => None,
        Err(e) => return Err(store_error_to_http(e)),
    };

    // Stop / Restart only need the instance id; skip the full
    // resolve so a vanished image or NIC doesn't block the
    // agent from acting on a still-existing zone.
    // Provision needs the full resolve (image, NICs, disks,
    // ssh keys) so the agent can build a vmadm payload.
    // Stop / Restart / Delete only need the instance id, which
    // is on `job.kind`, so we short-circuit and let the agent
    // act on the kind alone. Delete in particular runs *after*
    // the tritond record is gone, so the instance lookup
    // intentionally returns `instance: None`.
    let needs_full_resolve = matches!(job.kind, JobKind::Provision { .. });
    if !needs_full_resolve {
        return Ok(ProvisioningBlueprint {
            job_id: job.id,
            kind: job.kind.clone(),
            instance,
            image: None,
            nics: Vec::new(),
            subnets: Vec::new(),
            disks: Vec::new(),
            ssh_public_keys: Vec::new(),
            managed_identity: None,
        });
    }

    let Some(instance) = instance else {
        return Ok(ProvisioningBlueprint {
            job_id: job.id,
            kind: job.kind.clone(),
            instance: None,
            image: None,
            nics: Vec::new(),
            subnets: Vec::new(),
            disks: Vec::new(),
            ssh_public_keys: Vec::new(),
            managed_identity: None,
        });
    };

    let image = match store.get_image(instance.image_id).await {
        Ok(img) => Some(img),
        Err(StoreError::NotFound) => None,
        Err(e) => return Err(store_error_to_http(e)),
    };
    let nics = store
        .list_nics_for_instance(instance.id)
        .await
        .map_err(store_error_to_http)?;
    let mut subnets = Vec::new();
    for nic in &nics {
        if subnets
            .iter()
            .any(|subnet: &Subnet| subnet.id == nic.subnet_id)
        {
            continue;
        }
        let subnet = store
            .get_subnet(nic.subnet_id)
            .await
            .map_err(store_error_to_http)?;
        subnets.push(subnet);
    }
    let disks = store
        .list_disks_for_instance(instance.id)
        .await
        .map_err(store_error_to_http)?;

    let mut ssh_public_keys = Vec::with_capacity(instance.ssh_key_ids.len());
    for key_id in &instance.ssh_key_ids {
        // A key that vanished between instance create and job
        // claim is a transient inconsistency the agent shouldn't
        // crash on — skip and keep going.
        if let Ok(k) = store.get_ssh_key(*key_id).await {
            ssh_public_keys.push(k.public_key);
        }
    }

    let managed_identity = ManagedIdentity {
        instance_id: instance.id,
        tenant_id: instance.tenant_id,
        project_id: instance.project_id,
        identity_hmac: identity_hmac_key.sign(instance.id, instance.tenant_id, instance.project_id),
    };

    Ok(ProvisioningBlueprint {
        job_id: job.id,
        kind: job.kind.clone(),
        instance: Some(instance),
        image,
        nics,
        subnets,
        disks,
        ssh_public_keys,
        managed_identity: Some(managed_identity),
    })
}

/// Materialise the opaque Proteus `PortBlueprint` the bound CN agent
/// should apply for a NIC.
pub(crate) async fn build_port_blueprint(
    store: &dyn Store,
    port_id: Uuid,
    bound_cn: Uuid,
) -> Result<AgentPortBlueprint, HttpError> {
    let nic = store.get_nic(port_id).await.map_err(store_error_to_http)?;
    let instance = store
        .get_instance(nic.instance_id)
        .await
        .map_err(store_error_to_http)?;
    enforce_port_instance_available_to_bound_cn(store, &instance, bound_cn).await?;

    let project = store
        .get_project(nic.project_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant = store
        .get_tenant(nic.tenant_id)
        .await
        .map_err(store_error_to_http)?;
    let vpc = store
        .get_vpc(nic.vpc_id)
        .await
        .map_err(store_error_to_http)?;
    let subnet = store
        .get_subnet(nic.subnet_id)
        .await
        .map_err(store_error_to_http)?;

    if project.tenant_id != nic.tenant_id
        || tenant.id != nic.tenant_id
        || vpc.tenant_id != nic.tenant_id
        || vpc.project_id != nic.project_id
        || subnet.tenant_id != nic.tenant_id
        || subnet.project_id != nic.project_id
        || subnet.vpc_id != nic.vpc_id
        || instance.tenant_id != nic.tenant_id
        || instance.project_id != nic.project_id
    {
        return Err(not_found());
    }

    let routes = store
        .list_routes_in_table(subnet.route_table_id)
        .await
        .map_err(store_error_to_http)?;
    ensure_nat_gateway_edges_for_routes(store, &routes).await?;
    let nat_gateways = store
        .list_nat_gateways_in_vpc(vpc.id)
        .await
        .map_err(store_error_to_http)?;
    let edge_clusters = edge_clusters_for_nat_gateways(store, &nat_gateways).await?;
    let floating_ips = store
        .list_floating_ips_in_project(project.id)
        .await
        .map_err(store_error_to_http)?;
    // Slice 1 firewall: every rule scoped to the NIC's VPC flows into
    // the per-port intent. Group-based filtering lands later.
    let firewall_rules = store
        .list_firewall_rules_in_vpc(vpc.id)
        .await
        .map_err(store_error_to_http)?;

    let generation = INITIAL_PROTEUS_PORT_GENERATION;
    let intent = TritondPortIntentV1 {
        silo_id: tenant.silo_id,
        tenant_id: nic.tenant_id,
        project_id: nic.project_id,
        vpc: VpcIntentV1 {
            id: vpc.id,
            tenant_id: vpc.tenant_id,
            project_id: vpc.project_id,
            main_route_table_id: vpc.main_route_table_id,
            name: vpc.name,
            description: vpc.description,
            vni: vpc.vni,
            ipv4_block: vpc.ipv4_block.map(|cidr| cidr.to_string()),
            ipv6_block: vpc.ipv6_block.map(|cidr| cidr.to_string()),
        },
        subnet: SubnetIntentV1 {
            id: subnet.id,
            tenant_id: subnet.tenant_id,
            project_id: subnet.project_id,
            vpc_id: subnet.vpc_id,
            route_table_id: subnet.route_table_id,
            name: subnet.name,
            description: subnet.description,
            ipv4_block: subnet.ipv4_block.map(|cidr| cidr.to_string()),
            ipv6_block: subnet.ipv6_block.map(|cidr| cidr.to_string()),
        },
        nic: NicIntentV1 {
            id: nic.id,
            tenant_id: nic.tenant_id,
            project_id: nic.project_id,
            instance_id: nic.instance_id,
            vpc_id: nic.vpc_id,
            subnet_id: nic.subnet_id,
            name: nic.name,
            mac: nic.mac.clone(),
            primary_ipv4: nic.primary_ipv4.map(|addr| addr.to_string()),
            primary_ipv6: nic.primary_ipv6.map(|addr| addr.to_string()),
        },
        instance_id: instance.id,
        port_id,
        routes: routes
            .iter()
            .map(route_intent)
            .collect::<Result<Vec<_>, _>>()?,
        nat_gateways: nat_gateways.iter().map(nat_gateway_intent).collect(),
        floating_ips: floating_ips.iter().map(floating_ip_intent).collect(),
        edge_clusters: edge_clusters
            .iter()
            .map(edge_cluster_intent)
            .collect::<Result<Vec<_>, _>>()?,
        firewall_rules: firewall_rules.iter().map(firewall_rule_intent).collect(),
    };

    let plugin_blueprint = intent.compile_blueprint().map_err(|err| {
        store_error_to_http(StoreError::Conflict(format!(
            "port blueprint is not currently compilable: {err}"
        )))
    })?;
    let plugin_bytes = postcard::to_allocvec(&plugin_blueprint).map_err(|err| {
        HttpError::for_internal_error(format!("encode Triton VPC blueprint: {err}"))
    })?;
    let port_blueprint = PortBlueprint {
        port_id: ProteusPortId(port_id),
        network_id: ProteusNetworkId::TRITON_VPC,
        schema_version: PORT_BLUEPRINT_SCHEMA_V0,
        generation: ProteusGeneration::new(generation),
        limits: PortLimits::DEFAULT,
        link: ClientLinkConfig {
            mtu: 1500,
            mac_address: Some(parse_mac_bytes(&nic.mac)?),
            vlan_id: None,
        },
        plugin_config: PluginConfigBytes::new(
            ProteusNetworkId::TRITON_VPC,
            TRITON_VPC_BLUEPRINT_SCHEMA_V1,
            plugin_bytes,
        ),
    };
    let port_bytes = postcard::to_allocvec(&port_blueprint).map_err(|err| {
        HttpError::for_internal_error(format!("encode Proteus port blueprint: {err}"))
    })?;
    let blueprint_postcard_base64 = base64::engine::general_purpose::STANDARD.encode(port_bytes);

    Ok(AgentPortBlueprint {
        port_id,
        generation,
        blueprint_postcard_base64,
    })
}

pub(crate) async fn enforce_port_instance_available_to_bound_cn(
    store: &dyn Store,
    instance: &Instance,
    bound_cn: Uuid,
) -> Result<(), HttpError> {
    if instance.host_cn_uuid == Some(bound_cn) {
        return Ok(());
    }

    enforce_port_instance_claimed_by_bound_cn(store, instance.id, bound_cn).await
}

pub(crate) async fn enforce_port_instance_claimed_by_bound_cn(
    store: &dyn Store,
    instance_id: Uuid,
    bound_cn: Uuid,
) -> Result<(), HttpError> {
    let jobs = store
        .list_recent_jobs(1024)
        .await
        .map_err(store_error_to_http)?;
    for job in jobs
        .iter()
        .filter(|job| job.kind.instance_id() == Some(instance_id))
        .filter(|job| matches!(job.status, JobStatus::InProgress))
    {
        if enforce_job_belongs_to_bound_cn(job, bound_cn).is_ok() {
            return Ok(());
        }
    }

    Err(HttpError::for_client_error(
        Some("Forbidden".to_string()),
        ClientErrorStatusCode::FORBIDDEN,
        "bound key has no in-progress claim for this port's instance".to_string(),
    ))
}

pub(crate) fn route_intent(route: &Route) -> Result<RouteIntentV1, HttpError> {
    Ok(RouteIntentV1 {
        id: route.id,
        tenant_id: route.tenant_id,
        project_id: route.project_id,
        vpc_id: route.vpc_id,
        route_table_id: route.route_table_id,
        name: route.name.clone(),
        description: route.description.clone(),
        destination: route.destination.to_string(),
        target: route_target_intent(&route.target)?,
    })
}

pub(crate) fn route_target_intent(target: &RouteTarget) -> Result<RouteTargetIntentV1, HttpError> {
    match target {
        RouteTarget::Blackhole => Ok(RouteTargetIntentV1::Blackhole),
        RouteTarget::Reject => Ok(RouteTargetIntentV1::Reject),
        RouteTarget::VirtualGateway => Ok(RouteTargetIntentV1::VirtualGateway),
        RouteTarget::NatGateway { nat_gateway_id } => Ok(RouteTargetIntentV1::NatGateway {
            nat_gateway_id: *nat_gateway_id,
        }),
        RouteTarget::FloatingIp { floating_ip_id } => Ok(RouteTargetIntentV1::FloatingIp {
            floating_ip_id: *floating_ip_id,
        }),
        _ => Err(HttpError::for_internal_error(
            "unsupported route target variant in port blueprint compiler".to_string(),
        )),
    }
}

pub(crate) fn nat_gateway_intent(nat: &NatGateway) -> NatGatewayIntentV1 {
    NatGatewayIntentV1 {
        id: nat.id,
        tenant_id: nat.tenant_id,
        project_id: nat.project_id,
        vpc_id: nat.vpc_id,
        name: nat.name.clone(),
        description: nat.description.clone(),
        public_address: nat.public_address.to_string(),
        edge_cluster_id: nat.edge_cluster_id,
        desired_generation: nat.desired_generation,
    }
}

pub(crate) fn edge_cluster_intent(cluster: &EdgeCluster) -> Result<EdgeClusterIntentV1, HttpError> {
    Ok(EdgeClusterIntentV1 {
        id: cluster.id,
        underlay: cluster
            .instances
            .iter()
            .filter(|instance| {
                !matches!(
                    instance.state,
                    EdgeClusterInstanceState::Stopped | EdgeClusterInstanceState::Failed
                )
            })
            .filter_map(|instance| match instance.south_nic.ip {
                Some(IpAddr::V6(addr)) => Some(addr.to_string()),
                _ => None,
            })
            .collect(),
    })
}

pub(crate) fn floating_ip_intent(fip: &FloatingIp) -> FloatingIpIntentV1 {
    FloatingIpIntentV1 {
        id: fip.id,
        tenant_id: fip.tenant_id,
        project_id: fip.project_id,
        name: fip.name.clone(),
        description: fip.description.clone(),
        address: fip.address.to_string(),
        attached_to: fip
            .attached_to
            .as_ref()
            .map(|attachment| FloatingIpAttachmentIntentV1 {
                instance_id: attachment.instance_id,
                nic_id: attachment.nic_id,
            }),
        edge_cluster_id: None,
    }
}

/// Translate a tritond [`FirewallRule`] into the proteus per-port
/// intent shape. Used by [`build_port_blueprint`] to fold every rule
/// scoped to the NIC's VPC into the agent payload.
pub(crate) fn firewall_rule_intent(rule: &tritond_store::FirewallRule) -> FirewallRuleIntentV1 {
    FirewallRuleIntentV1 {
        id: rule.id,
        vpc_id: rule.vpc_id,
        name: rule.name.clone(),
        priority: rule.priority,
        direction: match rule.direction {
            tritond_store::FirewallDirection::Inbound => FirewallDirectionIntentV1::Inbound,
            tritond_store::FirewallDirection::Outbound => FirewallDirectionIntentV1::Outbound,
        },
        action: match rule.action {
            tritond_store::FirewallAction::Allow => FirewallActionIntentV1::Allow,
            tritond_store::FirewallAction::Deny => FirewallActionIntentV1::Deny,
        },
        protocol: match rule.protocol {
            tritond_store::FirewallProtocol::Any => L4ProtocolIntentV1::Any,
            tritond_store::FirewallProtocol::Tcp => L4ProtocolIntentV1::Tcp,
            tritond_store::FirewallProtocol::Udp => L4ProtocolIntentV1::Udp,
            tritond_store::FirewallProtocol::Icmp4 => L4ProtocolIntentV1::Icmp4,
            tritond_store::FirewallProtocol::Icmp6 => L4ProtocolIntentV1::Icmp6,
        },
        source_cidr: rule.source_cidr.map(|cidr| cidr.to_string()),
        destination_cidr: rule.destination_cidr.map(|cidr| cidr.to_string()),
        source_ports: rule.source_ports.map(|r| PortRangeIntentV1 {
            low: r.low,
            high: r.high,
        }),
        destination_ports: rule.destination_ports.map(|r| PortRangeIntentV1 {
            low: r.low,
            high: r.high,
        }),
        icmp_type_code: rule.icmp_type_code.map(|f| (f.kind, f.code)),
    }
}

pub(crate) fn parse_mac_bytes(value: &str) -> Result<[u8; 6], HttpError> {
    let mut mac = [0u8; 6];
    let mut count = 0usize;
    for (idx, part) in value.split(':').enumerate() {
        if idx >= mac.len() || part.len() != 2 {
            return Err(invalid_stored_mac(value));
        }
        mac[idx] = u8::from_str_radix(part, 16).map_err(|_| invalid_stored_mac(value))?;
        count += 1;
    }
    if count != mac.len() {
        return Err(invalid_stored_mac(value));
    }
    Ok(mac)
}

pub(crate) fn invalid_stored_mac(value: &str) -> HttpError {
    HttpError::for_internal_error(format!("stored NIC has invalid MAC address {value:?}"))
}
