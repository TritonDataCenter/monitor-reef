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
use triton_vpc::tritond_intent_v1::{
    DhcpOptionRawV1, DhcpOptionsIntentV1, EdgeClusterIntentV1, FirewallActionIntentV1,
    FirewallDirectionIntentV1, FirewallRuleIntentV1, FloatingIpAttachmentIntentV1,
    FloatingIpIntentV1, L4ProtocolIntentV1, NatGatewayIntentV1, NicIntentV1, PeerIntentV1,
    PortRangeIntentV1, RouteIntentV1, RouteTargetIntentV1, SubnetIntentV1, TritondPortIntentV1,
    VpcIntentV1,
};
use tritond_api::types::ImdsBindingWire;
use tritond_api::types::{
    FloatingIp, Instance, JobKind, JobStatus, ManagedIdentity, NatGateway, ProvisioningJob, Route,
    RouteTarget, Subnet,
};
use tritond_api::{AgentPortBlueprint, ProvisioningBlueprint};
use tritond_store::{
    DhcpOptionRaw, DhcpPool, DhcpReservation, EdgeCluster, EdgeClusterInstanceState, Store,
    StoreError,
};

use crate::cn_credential::enforce_job_belongs_to_bound_cn;
use crate::edge_cluster::{edge_clusters_for_nat_gateways, ensure_nat_gateway_edges_for_routes};
use crate::error::{not_found, store_error_to_http};
use crate::imds_config::{ImdsListenerConfig, pseudo_src_for_port};
use crate::realized_meta::build_instance_realized_view;

/// A concurrent delete after the job was claimed surfaces as
/// `instance: None` instead of 404; the agent then reports
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
            imds_bindings: Vec::new(),
            provision_metadata: Vec::new(),
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
            imds_bindings: Vec::new(),
            provision_metadata: Vec::new(),
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
            imds_bindings: Vec::new(),
            provision_metadata: Vec::new(),
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

    // IMDS bindings -- one per NIC when the cluster has IMDS wired
    // AND this instance's realized `config/imds/enabled` allows it.
    // Skipped silently when env vars are unset (IMDS not wired) so
    // pre-IMDS deployments keep their `imds_bindings: []` behaviour.
    let imds_bindings = build_imds_bindings_for_instance(store, instance.id, &nics).await;

    // Instance-scope metadata to fold into the vmadm payload's
    // customer_metadata / internal_metadata maps. Pull straight from
    // the instance's stored entries -- we only care about
    // `triton/instance/*` here (operator-set, this-VM-only),
    // not the layered config/state values; those flow via IMDS HTTP
    // at runtime and don't need to be in the cidata seed.
    let provision_metadata = build_provision_metadata(store, instance.id).await;

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
        imds_bindings,
        provision_metadata,
    })
}

async fn build_provision_metadata(
    store: &dyn Store,
    instance_id: Uuid,
) -> Vec<tritond_api::MetaEntry> {
    let entries = match store
        .list_meta(tritond_store::MetaScope::Instance, instance_id)
        .await
    {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    entries
        .into_iter()
        .filter(|(k, _v)| k.starts_with("instance/"))
        .map(|(k, v)| tritond_api::MetaEntry { key: k, value: v })
        .collect()
}

/// Returns empty when cluster IMDS is unwired or the instance has it
/// disabled; degrades silently if the realized-view fetch fails so a
/// bad metadata blob can't block provisioning.
async fn build_imds_bindings_for_instance(
    store: &dyn Store,
    instance_id: Uuid,
    nics: &[tritond_store::Nic],
) -> Vec<ImdsBindingWire> {
    if ImdsListenerConfig::from_env().is_none() {
        return Vec::new();
    }
    let view = match build_instance_realized_view(store, instance_id).await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let default_enabled = store
        .get_settings()
        .await
        .map(|s| s.imds_enabled_default)
        .unwrap_or(tritond_store::DEFAULT_IMDS_ENABLED);
    if !view.imds_enabled(default_enabled) {
        return Vec::new();
    }
    nics.iter()
        .map(|nic| ImdsBindingWire {
            pseudo_src: pseudo_src_for_port(nic.id),
            port_id: nic.id,
            instance_id,
        })
        .collect()
}

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

    // DHCP: the per-VPC pool (lease cadence + VPC-wide raw options) and
    // the reservation (if any) covering this NIC's MAC (hostname + per-MAC
    // raw options). Either being absent is normal — compile-time defaults
    // apply. The proteus compiler enforces the wire caps, so a malformed
    // pool config never produces an un-appliable blueprint.
    let dhcp_pool = store
        .get_dhcp_pool(vpc.id)
        .await
        .map_err(store_error_to_http)?;
    let dhcp_reservation = match store.get_dhcp_reservation(vpc.id, &nic.mac).await {
        Ok(r) => Some(r),
        Err(StoreError::NotFound) => None,
        Err(e) => return Err(store_error_to_http(e)),
    };
    let dhcp = dhcp_options_intent(&nic, dhcp_pool.as_ref(), dhcp_reservation.as_ref());

    // IMDS wiring: when the cluster has the listener configured and
    // this instance's realized `config/imds/enabled` is true, attach a
    // synthesized `LocalImds` route on the 169.254.169.254/32 magic
    // address and populate the per-port `imds` binding. The route
    // entry alone wouldn't fire the kmod NAT compile -- the binding
    // is what flips the schema to V2 and gives the compiler the
    // listener address to DNAT into. Both are skipped when IMDS is
    // disabled cluster-wide or per-instance, leaving the dataplane
    // exactly where it was pre-IM-3.
    // Peer table for intra-VPC cross-CN forwarding. Enumerate every
    // other realized NIC in the same subnet, look up its host CN, and
    // emit one PeerIntentV1 per peer pointing at the CN's underlay
    // address. The plugin compiler turns this into one Geneve push
    // rule per peer; a single `LocalSubnet` route in the route table
    // drives the fan-out.
    //
    // Failures listing other NICs / CNs degrade to an empty peer
    // table rather than failing the whole blueprint -- intra-VPC
    // traffic falls back to the existing behaviour (no encap, no
    // delivery) but provisioning still succeeds.
    let peers = build_peers_in_subnet(store, nic.project_id, nic.subnet_id, nic.id).await;

    let imds_cfg = ImdsListenerConfig::from_env();
    let imds_enabled = match imds_cfg {
        Some(_) => {
            // Cluster-default for `config/imds/enabled` lives in
            // Settings now (was a hardcoded constant); fetch it
            // alongside the realized view so an unset realized value
            // falls back to the operator-tunable cluster default.
            let default_enabled = store
                .get_settings()
                .await
                .map(|s| s.imds_enabled_default)
                .unwrap_or(tritond_store::DEFAULT_IMDS_ENABLED);
            build_instance_realized_view(store, instance.id)
                .await
                .map(|v| v.imds_enabled(default_enabled))
                .unwrap_or(false)
        }
        None => false,
    };

    // Per-port monotonic generation. A never-bumped port reads 1
    // (the historical provision baseline); a blueprint-affecting
    // mutation bumps it via `Store::bump_port_generation` so a
    // running-VM re-apply lands at a strictly-greater generation
    // instead of being swallowed as a same-generation no-op.
    let generation = store
        .get_port_generation(port_id)
        .await
        .map_err(store_error_to_http)?;
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
        routes: build_routes_with_imds_and_local_subnet(
            &routes,
            subnet.route_table_id,
            imds_enabled,
            subnet.ipv4_block.map(|c| c.to_string()),
            !peers.is_empty(),
        )?,
        nat_gateways: nat_gateways.iter().map(nat_gateway_intent).collect(),
        floating_ips: floating_ips.iter().map(floating_ip_intent).collect(),
        edge_clusters: edge_clusters
            .iter()
            .map(edge_cluster_intent)
            .collect::<Result<Vec<_>, _>>()?,
        firewall_rules: firewall_rules.iter().map(firewall_rule_intent).collect(),
        dhcp,
        imds: match (imds_cfg, imds_enabled) {
            (Some(cfg), true) => Some(triton_vpc::tritond_intent_v1::ImdsBindingIntentV1 {
                pseudo_src: pseudo_src_for_port(port_id),
                instance_id: instance.id,
                listener_ip: cfg.listener_ip,
                listener_port: cfg.listener_port,
            }),
            _ => None,
        },
        peers,
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
            // Mirror what the intent compile actually produced.
            // V2 is required when bp.imds is Some (the extra field
            // at the end of the postcard payload would fail a
            // strict V1 decode in the kmod); V1 stays correct for
            // IMDS-off blueprints. The intent layer is the single
            // authoritative source for which shape was emitted.
            plugin_blueprint.schema_version,
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

/// Translate the per-subnet stored routes into the intent shape and
/// (when IMDS is enabled for this instance) splice in a synthetic
/// `169.254.169.254/32 -> LocalImds` entry on the same route table.
///
/// The route doesn't exist in the store -- it's a property of the
/// dataplane wire, not user-configurable routing -- so we don't write
/// it to FDB; we just hand it to the kmod's compile step every time
/// the port blueprint is built. Stable UUID derived from the route
/// table id so repeated builds round-trip bit-identical.
fn build_routes_with_imds(
    stored: &[Route],
    route_table_id: Uuid,
    imds_enabled: bool,
) -> Result<Vec<RouteIntentV1>, HttpError> {
    let mut out: Vec<RouteIntentV1> = stored
        .iter()
        .map(route_intent)
        .collect::<Result<Vec<_>, _>>()?;
    if imds_enabled {
        // Deterministic UUID so re-emits are bit-identical.
        let synthetic_id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            &derive_imds_route_seed(route_table_id),
        );
        out.push(RouteIntentV1 {
            id: synthetic_id,
            tenant_id: Uuid::nil(),
            project_id: Uuid::nil(),
            vpc_id: Uuid::nil(),
            route_table_id,
            name: "imds-v4-magic".to_string(),
            description: "Synthesized: 169.254.169.254/32 -> LocalImds. IMDS_DESIGN.md §2.1."
                .to_string(),
            destination: "169.254.169.254/32".to_string(),
            target: RouteTargetIntentV1::LocalImds,
        });
    }
    Ok(out)
}

fn derive_imds_route_seed(route_table_id: Uuid) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(route_table_id.as_bytes());
    out[16..].copy_from_slice(b"imds-v4-magic\0\0\0");
    out
}

/// `LocalSubnet` is a dataplane property, not user-configurable, so
/// it isn't persisted; the overlay compiler fans it out per-peer
/// using `peer_table`.
fn build_routes_with_imds_and_local_subnet(
    stored: &[Route],
    route_table_id: Uuid,
    imds_enabled: bool,
    subnet_ipv4_block: Option<String>,
    emit_local_subnet: bool,
) -> Result<Vec<RouteIntentV1>, HttpError> {
    let mut out = build_routes_with_imds(stored, route_table_id, imds_enabled)?;
    if emit_local_subnet {
        if let Some(cidr) = subnet_ipv4_block {
            let synthetic_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                &derive_local_subnet_seed(route_table_id),
            );
            out.push(RouteIntentV1 {
                id: synthetic_id,
                tenant_id: Uuid::nil(),
                project_id: Uuid::nil(),
                vpc_id: Uuid::nil(),
                route_table_id,
                name: "local-subnet".to_string(),
                description: "Synthesized: subnet CIDR -> LocalSubnet (peer_table fan-out)"
                    .to_string(),
                destination: cidr,
                target: RouteTargetIntentV1::LocalSubnet,
            });
        }
    }
    Ok(out)
}

fn derive_local_subnet_seed(route_table_id: Uuid) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(route_table_id.as_bytes());
    out[16..].copy_from_slice(b"local-subnet\0\0\0\0");
    out
}

/// Re-declared (not re-exported from proteus_api) so tritond can drift
/// independently — e.g. shorter in dev to surface migration bugs.
const PEER_RESOLVE_DEFAULT_TTL_SECS: u32 = 300;

/// Phase A resolver: find the NIC whose primary IP matches `peer_ip`
/// inside a VPC with the given VNI, then look up its host CN's
/// underlay address. Brute-force scan over realized NICs; an index
/// lands when scale demands it.
///
/// Returns `Ok(response)` on success, `Err(404)` when no realized
/// NIC owns the IP / has a placed host CN with an admin IP. The
/// agent populates a negative-cache entry on `Err(404)` so the next
/// guest retry doesn't re-fire the slow path immediately.
pub(crate) async fn resolve_peer(
    store: &dyn Store,
    vni: u32,
    peer_ip: std::net::IpAddr,
) -> Result<tritond_api::AgentPeerResolveResponse, HttpError> {
    // Brute-force walk over realized NICs. Cost scales with placed-NIC
    // count, not configuration surface. Replace with a (vni, ip) index
    // when scale demands it.
    let cns = store.list_cns(None).await.map_err(store_error_to_http)?;
    for cn in cns {
        let Some(admin_ip) = cn.admin_ip else {
            continue;
        };
        let instances = match store.list_instances_for_cn(cn.server_uuid).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        for instance in instances {
            let nics = match store.list_nics_for_instance(instance.id).await {
                Ok(n) => n,
                Err(_) => continue,
            };
            for nic in nics {
                let nic_matches = match peer_ip {
                    std::net::IpAddr::V4(v4) => nic.primary_ipv4 == Some(v4),
                    std::net::IpAddr::V6(v6) => nic.primary_ipv6 == Some(v6),
                };
                if !nic_matches {
                    continue;
                }
                // v2p queries are tenant-bounded: mismatched (vni, ip) is 404.
                let vpc = match store.get_vpc(nic.vpc_id).await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if vpc.vni != vni {
                    continue;
                }
                return Ok(tritond_api::AgentPeerResolveResponse {
                    guest_mac: nic.mac.clone(),
                    underlay: underlay_v6_from_admin_ip(admin_ip),
                    ttl_seconds: PEER_RESOLVE_DEFAULT_TTL_SECS,
                });
            }
        }
    }
    Err(HttpError::for_client_error(
        Some("NotFound".to_string()),
        dropshot::ClientErrorStatusCode::NOT_FOUND,
        format!("no realized NIC owns peer {peer_ip} in vni {vni}"),
    ))
}

/// Returns empty on any store error: intra-VPC reachability is an
/// optimisation, not a precondition for provisioning. Unplaced
/// instances or CNs without an admin IP are silently skipped (not
/// reachable from the dataplane yet).
async fn build_peers_in_subnet(
    store: &dyn Store,
    project_id: Uuid,
    subnet_id: Uuid,
    self_nic_id: Uuid,
) -> Vec<PeerIntentV1> {
    let instances = match store.list_instances_in_project(project_id).await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut peers = Vec::new();
    for instance in instances {
        let Some(host_cn_uuid) = instance.host_cn_uuid else {
            continue;
        };
        let cn = match store.get_cn(host_cn_uuid).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let Some(admin_ip) = cn.admin_ip else {
            continue;
        };
        let nics = match store.list_nics_for_instance(instance.id).await {
            Ok(n) => n,
            Err(_) => continue,
        };
        for nic in nics {
            if nic.subnet_id != subnet_id {
                continue;
            }
            if nic.id == self_nic_id {
                continue;
            }
            let Some(addr) = nic.primary_ipv4.map(|v| v.to_string()) else {
                continue;
            };
            peers.push(PeerIntentV1 {
                addr,
                guest_mac: nic.mac.clone(),
                underlay: underlay_v6_from_admin_ip(admin_ip),
            });
        }
    }
    peers
}

/// Deterministic ULA encoding until CNs persist an explicit underlay
/// address at registration. Operators still configure the matching
/// IPv6 on the underlay link via SetUnderlay.
fn underlay_v6_from_admin_ip(v4: std::net::Ipv4Addr) -> String {
    let o = v4.octets();
    std::net::Ipv6Addr::new(
        0xfd00,
        0xcabe,
        0,
        0,
        0,
        0,
        ((o[0] as u16) << 8) | o[1] as u16,
        ((o[2] as u16) << 8) | o[3] as u16,
    )
    .to_string()
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

/// Pool options precede reservation options in the raw-option list;
/// the proteus compiler enforces length / per-value-size limits.
pub(crate) fn dhcp_options_intent(
    nic: &tritond_store::Nic,
    pool: Option<&DhcpPool>,
    reservation: Option<&DhcpReservation>,
) -> Option<DhcpOptionsIntentV1> {
    // Without an IPv4 the gateway never synthesises DHCP, and with no
    // pool and no reservation there is nothing to override — let the
    // compiler defaults stand.
    if nic.primary_ipv4.is_none() && pool.is_none() && reservation.is_none() {
        return None;
    }

    const DEFAULT_LEASE_SECONDS: u32 = 86_400;
    let lease_seconds = pool
        .map(|p| p.lease_seconds_default)
        .unwrap_or(DEFAULT_LEASE_SECONDS);
    let hostname = reservation.and_then(|r| r.hostname.clone());
    let mut additional_options: Vec<DhcpOptionRawV1> = Vec::new();
    if let Some(p) = pool {
        additional_options.extend(p.additional_options.iter().map(dhcp_option_raw));
    }
    if let Some(r) = reservation {
        additional_options.extend(r.per_mac_options.iter().map(dhcp_option_raw));
    }

    Some(DhcpOptionsIntentV1 {
        lease_seconds,
        hostname,
        additional_options,
    })
}

fn dhcp_option_raw(opt: &DhcpOptionRaw) -> DhcpOptionRawV1 {
    DhcpOptionRawV1 {
        code: opt.code,
        value: opt.value.clone(),
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

#[cfg(test)]
mod imds_tests {
    use super::*;
    use tritond_api::types::RouteTarget as ApiRouteTarget;

    fn stored_route(name: &str) -> Route {
        Route {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            vpc_id: Uuid::new_v4(),
            route_table_id: Uuid::new_v4(),
            name: name.into(),
            description: String::new(),
            destination: "0.0.0.0/0".parse().unwrap(),
            target: ApiRouteTarget::VirtualGateway,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn build_routes_with_imds_off_returns_stored_only() {
        let stored = vec![stored_route("default")];
        let out =
            build_routes_with_imds(&stored, stored[0].route_table_id, false).expect("compile");
        assert_eq!(out.len(), 1);
        assert!(
            !matches!(out[0].target, RouteTargetIntentV1::LocalImds),
            "no synthetic LocalImds when imds_enabled = false"
        );
    }

    #[test]
    fn build_routes_with_imds_on_appends_local_imds_route() {
        let stored = vec![stored_route("default")];
        let route_table_id = stored[0].route_table_id;
        let out = build_routes_with_imds(&stored, route_table_id, true).expect("compile");
        assert_eq!(out.len(), 2, "stored + synthetic IMDS route");
        let imds_route = out
            .iter()
            .find(|r| matches!(r.target, RouteTargetIntentV1::LocalImds))
            .expect("LocalImds route present when imds_enabled");
        assert_eq!(imds_route.destination, "169.254.169.254/32");
        assert_eq!(imds_route.route_table_id, route_table_id);
        // The synthetic route id is deterministic -- a second call
        // round-trips bit-identical so the blueprint cache key
        // stays stable.
        let out2 = build_routes_with_imds(&stored, route_table_id, true).expect("compile");
        let imds_route2 = out2
            .iter()
            .find(|r| matches!(r.target, RouteTargetIntentV1::LocalImds))
            .expect("second build still has LocalImds");
        assert_eq!(imds_route.id, imds_route2.id);
    }

    #[test]
    fn build_routes_with_imds_emits_distinct_id_per_route_table() {
        // Two distinct route tables produce distinct synthetic-route
        // UUIDs -- proves the seed actually folds in the route table
        // id rather than collapsing to a fixed value.
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let out_a = build_routes_with_imds(&[], a, true).expect("compile");
        let out_b = build_routes_with_imds(&[], b, true).expect("compile");
        let id_a = out_a
            .iter()
            .find(|r| matches!(r.target, RouteTargetIntentV1::LocalImds))
            .unwrap()
            .id;
        let id_b = out_b
            .iter()
            .find(|r| matches!(r.target, RouteTargetIntentV1::LocalImds))
            .unwrap()
            .id;
        assert_ne!(id_a, id_b);
    }
}
