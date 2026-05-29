// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FoundationDB key builders.
//!
//! Single source of truth for every key shape the [`super::FdbStore`]
//! reads or writes. The schema doc lives in `super`; this module is
//! the canonical implementation. Functions are `pub(super)` because
//! callers outside the backend module have no business hand-rolling
//! keys.

use ipnetwork::IpNetwork;
use uuid::Uuid;

use crate::{CnState, EdgeClusterResource, MetaScope, NetworkResourceId, RealizerId, SystemKey};

pub(super) fn silo_by_id_key(id: Uuid) -> Vec<u8> {
    format!("silo/by_id/{id}").into_bytes()
}

pub(super) fn silo_by_name_key(name: &str) -> Vec<u8> {
    format!("silo/by_name/{name}").into_bytes()
}

pub(super) fn tenant_by_id_key(id: Uuid) -> Vec<u8> {
    format!("tenant/by_id/{id}").into_bytes()
}

pub(super) fn tenant_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
    format!("tenant/by_silo/{silo_id}/{name}").into_bytes()
}

pub(super) fn tenant_in_silo_key(silo_id: Uuid, tenant_id: Uuid) -> Vec<u8> {
    format!("tenant/in_silo/{silo_id}/{tenant_id}").into_bytes()
}

pub(super) fn tenant_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
    format!("tenant/in_silo/{silo_id}/").into_bytes()
}

pub(super) fn user_by_id_key(id: Uuid) -> Vec<u8> {
    format!("user/by_id/{id}").into_bytes()
}

pub(super) fn user_by_name_key(name: &str) -> Vec<u8> {
    format!("user/by_name/{name}").into_bytes()
}

pub(super) fn user_prefix() -> &'static [u8] {
    b"user/by_id/"
}

pub(super) fn apikey_by_id_key(id: Uuid) -> Vec<u8> {
    format!("apikey/by_id/{id}").into_bytes()
}

pub(super) fn apikey_by_lookup_key(lookup_id: &str) -> Vec<u8> {
    format!("apikey/by_lookup/{lookup_id}").into_bytes()
}

pub(super) fn apikey_user_index_key(user_id: Uuid, key_id: Uuid) -> Vec<u8> {
    format!("apikey/by_user/{user_id}/{key_id}").into_bytes()
}

pub(super) fn apikey_user_index_prefix(user_id: Uuid) -> Vec<u8> {
    format!("apikey/by_user/{user_id}/").into_bytes()
}

pub(super) fn system_key(key: SystemKey) -> Vec<u8> {
    format!("system/{}", key.tag()).into_bytes()
}

pub(super) fn user_federation_key(tenant_id: Uuid, issuer: &str, subject: &str) -> Vec<u8> {
    // SHA-256 of `issuer\0subject` → fixed-length, no escaping
    // worries for arbitrary issuer URLs that contain slashes.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(issuer.as_bytes());
    hasher.update(b"\0");
    hasher.update(subject.as_bytes());
    let digest = hasher.finalize();
    let hex = digest_to_hex(&digest);
    format!("user/by_federation/{tenant_id}/{hex}").into_bytes()
}

pub(super) fn idp_config_key(tenant_id: Uuid) -> Vec<u8> {
    format!("idp/by_tenant/{tenant_id}").into_bytes()
}

pub(super) fn idp_config_prefix() -> &'static [u8] {
    b"idp/by_tenant/"
}

pub(super) fn idp_by_issuer_key(issuer: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(issuer.as_bytes());
    let digest = hasher.finalize();
    let hex = digest_to_hex(&digest);
    format!("idp/by_issuer/{hex}").into_bytes()
}

pub(super) fn project_by_id_key(id: Uuid) -> Vec<u8> {
    format!("project/by_id/{id}").into_bytes()
}

pub(super) fn project_by_tenant_name_key(tenant_id: Uuid, name: &str) -> Vec<u8> {
    format!("project/by_tenant/{tenant_id}/{name}").into_bytes()
}

pub(super) fn project_in_tenant_key(tenant_id: Uuid, project_id: Uuid) -> Vec<u8> {
    format!("project/in_tenant/{tenant_id}/{project_id}").into_bytes()
}

pub(super) fn project_in_tenant_prefix(tenant_id: Uuid) -> Vec<u8> {
    format!("project/in_tenant/{tenant_id}/").into_bytes()
}

pub(super) fn vpc_by_id_key(id: Uuid) -> Vec<u8> {
    format!("vpc/by_id/{id}").into_bytes()
}

pub(super) fn vpc_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
    format!("vpc/by_project/{project_id}/{name}").into_bytes()
}

pub(super) fn vpc_in_project_key(project_id: Uuid, vpc_id: Uuid) -> Vec<u8> {
    format!("vpc/in_project/{project_id}/{vpc_id}").into_bytes()
}

pub(super) fn vpc_in_project_prefix(project_id: Uuid) -> Vec<u8> {
    format!("vpc/in_project/{project_id}/").into_bytes()
}

pub(super) fn vpc_by_vni_key(vni: u32) -> Vec<u8> {
    format!("vpc/by_vni/{vni:08x}").into_bytes()
}

pub(super) fn subnet_by_id_key(id: Uuid) -> Vec<u8> {
    format!("subnet/by_id/{id}").into_bytes()
}

pub(super) fn subnet_by_vpc_name_key(vpc_id: Uuid, name: &str) -> Vec<u8> {
    format!("subnet/by_vpc/{vpc_id}/{name}").into_bytes()
}

pub(super) fn subnet_in_vpc_key(vpc_id: Uuid, subnet_id: Uuid) -> Vec<u8> {
    format!("subnet/in_vpc/{vpc_id}/{subnet_id}").into_bytes()
}

pub(super) fn subnet_in_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
    format!("subnet/in_vpc/{vpc_id}/").into_bytes()
}

pub(super) fn route_table_by_id_key(id: Uuid) -> Vec<u8> {
    format!("route_table/by_id/{id}").into_bytes()
}

pub(super) fn route_table_by_vpc_name_key(vpc_id: Uuid, name: &str) -> Vec<u8> {
    format!("route_table/by_vpc/{vpc_id}/{name}").into_bytes()
}

pub(super) fn route_table_in_vpc_key(vpc_id: Uuid, route_table_id: Uuid) -> Vec<u8> {
    format!("route_table/in_vpc/{vpc_id}/{route_table_id}").into_bytes()
}

pub(super) fn route_table_in_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
    format!("route_table/in_vpc/{vpc_id}/").into_bytes()
}

pub(super) fn route_table_main_key(vpc_id: Uuid) -> Vec<u8> {
    format!("route_table/main/{vpc_id}").into_bytes()
}

pub(super) fn route_by_id_key(id: Uuid) -> Vec<u8> {
    format!("route/by_id/{id}").into_bytes()
}

pub(super) fn route_by_id_prefix() -> Vec<u8> {
    b"route/by_id/".to_vec()
}

pub(super) fn route_by_table_destination_key(route_table_id: Uuid, destination: IpNetwork) -> Vec<u8> {
    format!("route/by_table/{route_table_id}/{destination}").into_bytes()
}

pub(super) fn route_in_table_key(route_table_id: Uuid, route_id: Uuid) -> Vec<u8> {
    format!("route/in_table/{route_table_id}/{route_id}").into_bytes()
}

pub(super) fn route_in_table_prefix(route_table_id: Uuid) -> Vec<u8> {
    format!("route/in_table/{route_table_id}/").into_bytes()
}

pub(super) fn nat_gateway_by_id_key(id: Uuid) -> Vec<u8> {
    format!("nat_gateway/by_id/{id}").into_bytes()
}

pub(super) fn nat_gateway_by_vpc_name_key(vpc_id: Uuid, name: &str) -> Vec<u8> {
    format!("nat_gateway/by_vpc/{vpc_id}/{name}").into_bytes()
}

pub(super) fn nat_gateway_in_vpc_key(vpc_id: Uuid, nat_gateway_id: Uuid) -> Vec<u8> {
    format!("nat_gateway/in_vpc/{vpc_id}/{nat_gateway_id}").into_bytes()
}

pub(super) fn nat_gateway_in_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
    format!("nat_gateway/in_vpc/{vpc_id}/").into_bytes()
}

pub(super) fn edge_cluster_by_id_key(id: Uuid) -> Vec<u8> {
    format!("edge_cluster/by_id/{id}").into_bytes()
}

pub(super) fn edge_cluster_by_name_key(name: &str) -> Vec<u8> {
    format!("edge_cluster/by_name/{name}").into_bytes()
}

pub(super) fn edge_cluster_all_key(id: Uuid) -> Vec<u8> {
    format!("edge_cluster/all/{id}").into_bytes()
}

pub(super) fn edge_cluster_all_prefix() -> &'static [u8] {
    b"edge_cluster/all/"
}

pub(super) fn edge_cluster_by_resource_key(resource: EdgeClusterResource, id: Uuid) -> Vec<u8> {
    format!(
        "edge_cluster/by_resource/{}/{}/{id}",
        resource.kind_tag(),
        resource.id()
    )
    .into_bytes()
}

pub(super) fn edge_cluster_by_resource_prefix(resource: EdgeClusterResource) -> Vec<u8> {
    format!(
        "edge_cluster/by_resource/{}/{}/",
        resource.kind_tag(),
        resource.id()
    )
    .into_bytes()
}

pub(super) fn storage_cluster_by_id_key(id: Uuid) -> Vec<u8> {
    format!("storage_cluster/by_id/{id}").into_bytes()
}

pub(super) fn storage_cluster_by_name_key(name: &str) -> Vec<u8> {
    format!("storage_cluster/by_name/{name}").into_bytes()
}

pub(super) fn storage_cluster_all_key(id: Uuid) -> Vec<u8> {
    format!("storage_cluster/all/{id}").into_bytes()
}

pub(super) fn storage_cluster_all_prefix() -> &'static [u8] {
    b"storage_cluster/all/"
}

pub(super) fn ssh_key_by_id_key(id: Uuid) -> Vec<u8> {
    format!("ssh_key/by_id/{id}").into_bytes()
}

pub(super) fn ssh_key_by_public_name_key(name: &str) -> Vec<u8> {
    format!("ssh_key/by_public/{name}").into_bytes()
}

pub(super) fn ssh_key_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
    format!("ssh_key/by_silo/{silo_id}/{name}").into_bytes()
}

pub(super) fn ssh_key_by_tenant_name_key(tenant_id: Uuid, name: &str) -> Vec<u8> {
    format!("ssh_key/by_tenant/{tenant_id}/{name}").into_bytes()
}

pub(super) fn ssh_key_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
    format!("ssh_key/by_project/{project_id}/{name}").into_bytes()
}

pub(super) fn ssh_key_by_user_name_key(user_id: Uuid, name: &str) -> Vec<u8> {
    format!("ssh_key/by_user/{user_id}/{name}").into_bytes()
}

pub(super) fn ssh_key_by_public_fp_key(fingerprint: &str) -> Vec<u8> {
    format!("ssh_key/by_public_fp/{fingerprint}").into_bytes()
}

pub(super) fn ssh_key_by_silo_fp_key(silo_id: Uuid, fingerprint: &str) -> Vec<u8> {
    format!("ssh_key/by_silo_fp/{silo_id}/{fingerprint}").into_bytes()
}

pub(super) fn ssh_key_by_tenant_fp_key(tenant_id: Uuid, fingerprint: &str) -> Vec<u8> {
    format!("ssh_key/by_tenant_fp/{tenant_id}/{fingerprint}").into_bytes()
}

pub(super) fn ssh_key_by_project_fp_key(project_id: Uuid, fingerprint: &str) -> Vec<u8> {
    format!("ssh_key/by_project_fp/{project_id}/{fingerprint}").into_bytes()
}

pub(super) fn ssh_key_by_user_fp_key(user_id: Uuid, fingerprint: &str) -> Vec<u8> {
    format!("ssh_key/by_user_fp/{user_id}/{fingerprint}").into_bytes()
}

pub(super) fn ssh_key_in_public_key(key_id: Uuid) -> Vec<u8> {
    format!("ssh_key/in_public/{key_id}").into_bytes()
}

pub(super) fn ssh_key_in_public_prefix() -> Vec<u8> {
    b"ssh_key/in_public/".to_vec()
}

pub(super) fn ssh_key_in_silo_key(silo_id: Uuid, key_id: Uuid) -> Vec<u8> {
    format!("ssh_key/in_silo/{silo_id}/{key_id}").into_bytes()
}

pub(super) fn ssh_key_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
    format!("ssh_key/in_silo/{silo_id}/").into_bytes()
}

pub(super) fn ssh_key_in_tenant_key(tenant_id: Uuid, key_id: Uuid) -> Vec<u8> {
    format!("ssh_key/in_tenant/{tenant_id}/{key_id}").into_bytes()
}

pub(super) fn ssh_key_in_tenant_prefix(tenant_id: Uuid) -> Vec<u8> {
    format!("ssh_key/in_tenant/{tenant_id}/").into_bytes()
}

pub(super) fn ssh_key_in_project_key(project_id: Uuid, key_id: Uuid) -> Vec<u8> {
    format!("ssh_key/in_project/{project_id}/{key_id}").into_bytes()
}

pub(super) fn ssh_key_in_project_prefix(project_id: Uuid) -> Vec<u8> {
    format!("ssh_key/in_project/{project_id}/").into_bytes()
}

pub(super) fn ssh_key_by_user_idx_key(user_id: Uuid, key_id: Uuid) -> Vec<u8> {
    format!("ssh_key/by_user_idx/{user_id}/{key_id}").into_bytes()
}

pub(super) fn ssh_key_by_user_idx_prefix(user_id: Uuid) -> Vec<u8> {
    format!("ssh_key/by_user_idx/{user_id}/").into_bytes()
}

pub(super) fn image_by_id_key(id: Uuid) -> Vec<u8> {
    format!("image/by_id/{id}").into_bytes()
}

pub(super) fn image_by_public_name_key(name: &str) -> Vec<u8> {
    format!("image/by_public/{name}").into_bytes()
}

pub(super) fn image_by_silo_name_key(silo_id: Uuid, name: &str) -> Vec<u8> {
    format!("image/by_silo/{silo_id}/{name}").into_bytes()
}

pub(super) fn image_by_tenant_name_key(tenant_id: Uuid, name: &str) -> Vec<u8> {
    format!("image/by_tenant/{tenant_id}/{name}").into_bytes()
}

pub(super) fn image_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
    format!("image/by_project/{project_id}/{name}").into_bytes()
}

pub(super) fn image_by_user_name_key(user_id: Uuid, name: &str) -> Vec<u8> {
    format!("image/by_user/{user_id}/{name}").into_bytes()
}

pub(super) fn image_in_public_key(image_id: Uuid) -> Vec<u8> {
    format!("image/in_public/{image_id}").into_bytes()
}

pub(super) fn image_in_public_prefix() -> Vec<u8> {
    b"image/in_public/".to_vec()
}

pub(super) fn image_in_silo_key(silo_id: Uuid, image_id: Uuid) -> Vec<u8> {
    format!("image/in_silo/{silo_id}/{image_id}").into_bytes()
}

pub(super) fn image_in_silo_prefix(silo_id: Uuid) -> Vec<u8> {
    format!("image/in_silo/{silo_id}/").into_bytes()
}

pub(super) fn image_in_tenant_key(tenant_id: Uuid, image_id: Uuid) -> Vec<u8> {
    format!("image/in_tenant/{tenant_id}/{image_id}").into_bytes()
}

pub(super) fn image_in_tenant_prefix(tenant_id: Uuid) -> Vec<u8> {
    format!("image/in_tenant/{tenant_id}/").into_bytes()
}

pub(super) fn image_in_project_key(project_id: Uuid, image_id: Uuid) -> Vec<u8> {
    format!("image/in_project/{project_id}/{image_id}").into_bytes()
}

pub(super) fn image_in_project_prefix(project_id: Uuid) -> Vec<u8> {
    format!("image/in_project/{project_id}/").into_bytes()
}

pub(super) fn image_by_user_idx_key(user_id: Uuid, image_id: Uuid) -> Vec<u8> {
    format!("image/by_user_idx/{user_id}/{image_id}").into_bytes()
}

pub(super) fn image_by_user_idx_prefix(user_id: Uuid) -> Vec<u8> {
    format!("image/by_user_idx/{user_id}/").into_bytes()
}

pub(super) fn quota_by_project_key(project_id: Uuid) -> Vec<u8> {
    format!("quota/by_project/{project_id}").into_bytes()
}

pub(super) fn instance_by_id_key(id: Uuid) -> Vec<u8> {
    format!("instance/by_id/{id}").into_bytes()
}

pub(super) fn instance_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
    format!("instance/by_project/{project_id}/{name}").into_bytes()
}

pub(super) fn instance_in_project_key(project_id: Uuid, instance_id: Uuid) -> Vec<u8> {
    format!("instance/in_project/{project_id}/{instance_id}").into_bytes()
}

pub(super) fn instance_in_project_prefix(project_id: Uuid) -> Vec<u8> {
    format!("instance/in_project/{project_id}/").into_bytes()
}

pub(super) fn instance_in_host_cn_key(host_cn_uuid: Uuid, instance_id: Uuid) -> Vec<u8> {
    format!("instance/in_host_cn/{host_cn_uuid}/{instance_id}").into_bytes()
}

pub(super) fn instance_in_host_cn_prefix(host_cn_uuid: Uuid) -> Vec<u8> {
    format!("instance/in_host_cn/{host_cn_uuid}/").into_bytes()
}

pub(super) fn instance_in_image_key(image_id: Uuid, instance_id: Uuid) -> Vec<u8> {
    format!("instance/in_image/{image_id}/{instance_id}").into_bytes()
}

pub(super) fn instance_in_image_prefix(image_id: Uuid) -> Vec<u8> {
    format!("instance/in_image/{image_id}/").into_bytes()
}

pub(super) fn nic_by_id_key(id: Uuid) -> Vec<u8> {
    format!("nic/by_id/{id}").into_bytes()
}

/// Per-port monotonic proteus-blueprint generation counter. Bumped on
/// every blueprint-affecting mutation so a running VM's port can be
/// re-applied at a strictly-greater generation (the kmod no-ops a
/// re-apply at the same generation). `port_id` is the NIC id.
pub(super) fn port_generation_key(port_id: Uuid) -> Vec<u8> {
    format!("port-gen/{port_id}").into_bytes()
}

pub(super) fn nic_in_subnet_key(subnet_id: Uuid, nic_id: Uuid) -> Vec<u8> {
    format!("nic/in_subnet/{subnet_id}/{nic_id}").into_bytes()
}

pub(super) fn nic_in_subnet_prefix(subnet_id: Uuid) -> Vec<u8> {
    format!("nic/in_subnet/{subnet_id}/").into_bytes()
}

pub(super) fn nic_by_ip_key(ip: std::net::IpAddr) -> Vec<u8> {
    format!("nic/by_ip/{ip}").into_bytes()
}

pub(super) fn dhcp_lease_by_mac_key(mac: &str) -> Vec<u8> {
    format!("dhcp_lease/by_mac/{mac}").into_bytes()
}

pub(super) fn nic_in_instance_key(instance_id: Uuid, nic_id: Uuid) -> Vec<u8> {
    format!("nic/in_instance/{instance_id}/{nic_id}").into_bytes()
}

pub(super) fn nic_in_instance_prefix(instance_id: Uuid) -> Vec<u8> {
    format!("nic/in_instance/{instance_id}/").into_bytes()
}

pub(super) fn nic_ip_alloc_v4_key(subnet_id: Uuid, ip: std::net::Ipv4Addr) -> Vec<u8> {
    format!("nic/ip_alloc/{subnet_id}/v4/{ip}").into_bytes()
}

pub(super) fn nic_ip_alloc_v6_key(subnet_id: Uuid, ip: std::net::Ipv6Addr) -> Vec<u8> {
    format!("nic/ip_alloc/{subnet_id}/v6/{ip}").into_bytes()
}

pub(super) fn nic_ip_alloc_v4_prefix(subnet_id: Uuid) -> Vec<u8> {
    format!("nic/ip_alloc/{subnet_id}/v4/").into_bytes()
}

pub(super) fn nic_ip_alloc_v6_prefix(subnet_id: Uuid) -> Vec<u8> {
    format!("nic/ip_alloc/{subnet_id}/v6/").into_bytes()
}

pub(super) fn disk_by_id_key(id: Uuid) -> Vec<u8> {
    format!("disk/by_id/{id}").into_bytes()
}

pub(super) fn disk_in_instance_key(instance_id: Uuid, disk_id: Uuid) -> Vec<u8> {
    format!("disk/in_instance/{instance_id}/{disk_id}").into_bytes()
}

pub(super) fn disk_in_instance_prefix(instance_id: Uuid) -> Vec<u8> {
    format!("disk/in_instance/{instance_id}/").into_bytes()
}

pub(super) fn floating_ip_by_id_key(id: Uuid) -> Vec<u8> {
    format!("floating_ip/by_id/{id}").into_bytes()
}

pub(super) fn floating_ip_by_project_name_key(project_id: Uuid, name: &str) -> Vec<u8> {
    format!("floating_ip/by_project/{project_id}/{name}").into_bytes()
}

pub(super) fn floating_ip_in_project_key(project_id: Uuid, fip_id: Uuid) -> Vec<u8> {
    format!("floating_ip/in_project/{project_id}/{fip_id}").into_bytes()
}

pub(super) fn floating_ip_in_project_prefix(project_id: Uuid) -> Vec<u8> {
    format!("floating_ip/in_project/{project_id}/").into_bytes()
}

pub(super) fn floating_ip_alloc_v4_key(ip: std::net::Ipv4Addr) -> Vec<u8> {
    format!("floating_ip/alloc/v4/{ip}").into_bytes()
}

pub(super) fn floating_ip_alloc_v6_key(ip: std::net::Ipv6Addr) -> Vec<u8> {
    format!("floating_ip/alloc/v6/{ip}").into_bytes()
}

pub(super) fn floating_ip_alloc_v4_prefix() -> &'static [u8] {
    b"floating_ip/alloc/v4/"
}

pub(super) fn floating_ip_alloc_v6_prefix() -> &'static [u8] {
    b"floating_ip/alloc/v6/"
}

pub(super) fn public_ip_holder_value(resource: NetworkResourceId) -> Vec<u8> {
    format!("{}:{}", resource.kind_tag(), resource.id()).into_bytes()
}

pub(super) fn job_by_id_key(id: Uuid) -> Vec<u8> {
    format!("job/by_id/{id}").into_bytes()
}

pub(super) fn job_pending_key(seq: u64) -> Vec<u8> {
    // 16-char zero-padded hex so the FDB key sort matches the
    // numeric u64 sort. (Big-endian raw bytes would also work,
    // but the prefix `job/pending/` is utf8 so we stay readable.)
    format!("job/pending/{seq:016x}").into_bytes()
}

pub(super) fn job_pending_prefix() -> &'static [u8] {
    b"job/pending/"
}

pub(super) fn job_seq_counter_key() -> &'static [u8] {
    b"job/seq/counter"
}

pub(super) fn cn_by_uuid_key(server_uuid: Uuid) -> Vec<u8> {
    format!("cn/by_uuid/{server_uuid}").into_bytes()
}

pub(super) fn cn_by_claim_key(normalized_code: &str) -> Vec<u8> {
    format!("cn/by_claim/{normalized_code}").into_bytes()
}

pub(super) fn cn_by_poll_key(poll_token: &str) -> Vec<u8> {
    format!("cn/by_poll/{poll_token}").into_bytes()
}

pub(super) fn cn_by_state_key(state: CnState, server_uuid: Uuid) -> Vec<u8> {
    format!("cn/by_state/{}/{server_uuid}", cn_state_tag(state)).into_bytes()
}

pub(super) fn cn_by_state_prefix(state: CnState) -> Vec<u8> {
    format!("cn/by_state/{}/", cn_state_tag(state)).into_bytes()
}

pub(super) fn auto_approve_window_key() -> &'static [u8] {
    b"auto_approve/window"
}

pub(super) fn settings_key() -> &'static [u8] {
    b"config/settings"
}

pub(super) fn meta_scope_prefix(scope: MetaScope, scope_id: Uuid) -> Vec<u8> {
    format!("meta/{}/{}/", scope.as_str(), scope_id).into_bytes()
}

pub(super) fn meta_entry_key(scope: MetaScope, scope_id: Uuid, key: &str) -> Vec<u8> {
    let mut k = meta_scope_prefix(scope, scope_id);
    k.extend_from_slice(key.as_bytes());
    k
}

pub(super) fn meta_gen_key(scope: MetaScope, scope_id: Uuid) -> Vec<u8> {
    format!("meta/gen/{}/{}", scope.as_str(), scope_id).into_bytes()
}

pub(super) fn legacy_vm_by_id_key(smartos_uuid: Uuid) -> Vec<u8> {
    format!("legacy_vm/by_id/{smartos_uuid}").into_bytes()
}

pub(super) fn legacy_vm_in_host_cn_key(host_cn_uuid: Uuid, smartos_uuid: Uuid) -> Vec<u8> {
    format!("legacy_vm/in_host_cn/{host_cn_uuid}/{smartos_uuid}").into_bytes()
}

pub(super) fn legacy_vm_in_host_cn_prefix(host_cn_uuid: Uuid) -> Vec<u8> {
    format!("legacy_vm/in_host_cn/{host_cn_uuid}/").into_bytes()
}

pub(super) fn legacy_vm_by_id_prefix() -> &'static [u8] {
    b"legacy_vm/by_id/"
}

pub(super) fn network_realization_key(resource: NetworkResourceId, realizer: RealizerId) -> Vec<u8> {
    format!(
        "network_realization/{}/{}/{}/{}",
        resource.kind_tag(),
        resource.id(),
        realizer.kind_tag(),
        realizer.id(),
    )
    .into_bytes()
}

pub(super) fn network_realization_resource_prefix(resource: NetworkResourceId) -> Vec<u8> {
    format!(
        "network_realization/{}/{}/",
        resource.kind_tag(),
        resource.id(),
    )
    .into_bytes()
}

pub(super) fn dhcp_pool_by_vpc_key(vpc_id: Uuid) -> Vec<u8> {
    format!("dhcp_pool/by_vpc/{vpc_id}").into_bytes()
}

pub(super) fn dhcp_reservation_by_vpc_mac_key(vpc_id: Uuid, mac: &str) -> Vec<u8> {
    format!("dhcp_reservation/by_vpc/{vpc_id}/{mac}").into_bytes()
}

pub(super) fn dhcp_reservation_by_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
    format!("dhcp_reservation/by_vpc/{vpc_id}/").into_bytes()
}

pub(super) fn dhcp_lease_by_vpc_mac_key(vpc_id: Uuid, mac: &str) -> Vec<u8> {
    format!("dhcp_lease/by_vpc/{vpc_id}/{mac}").into_bytes()
}

pub(super) fn dhcp_lease_by_vpc_prefix(vpc_id: Uuid) -> Vec<u8> {
    format!("dhcp_lease/by_vpc/{vpc_id}/").into_bytes()
}

pub(super) fn dhcp_lease_global_prefix() -> &'static [u8] {
    b"dhcp_lease/by_vpc/"
}

pub(super) fn cn_capacity_key(server_uuid: Uuid) -> Vec<u8> {
    format!("cn_capacity/{server_uuid}").into_bytes()
}

pub(super) fn cn_capacity_prefix() -> &'static [u8] {
    b"cn_capacity/"
}

pub(super) fn cn_placement_key(server_uuid: Uuid) -> Vec<u8> {
    format!("cn_placement/{server_uuid}").into_bytes()
}

pub(super) fn cn_placement_prefix() -> &'static [u8] {
    b"cn_placement/"
}

pub(super) fn cn_reservation_key(server_uuid: Uuid, saga_id: Uuid) -> Vec<u8> {
    format!("cn_reservation/{server_uuid}/{saga_id}").into_bytes()
}

pub(super) fn cn_reservation_per_cn_prefix(server_uuid: Uuid) -> Vec<u8> {
    format!("cn_reservation/{server_uuid}/").into_bytes()
}

pub(super) fn cn_reservation_prefix() -> &'static [u8] {
    b"cn_reservation/"
}

pub(super) fn cn_load_summary_key(server_uuid: Uuid) -> Vec<u8> {
    format!("cn_load_summary/{server_uuid}").into_bytes()
}

pub(super) fn cn_load_summary_prefix() -> &'static [u8] {
    b"cn_load_summary/"
}

pub(super) fn instance_affinity_key(instance_id: Uuid) -> Vec<u8> {
    format!("instance_affinity/{instance_id}").into_bytes()
}

pub(super) fn instance_affinity_by_tenant_key(tenant_id: Uuid, instance_id: Uuid) -> Vec<u8> {
    format!("instance_affinity_by_tenant/{tenant_id}/{instance_id}").into_bytes()
}

pub(super) fn instance_affinity_by_tenant_prefix(tenant_id: Uuid) -> Vec<u8> {
    format!("instance_affinity_by_tenant/{tenant_id}/").into_bytes()
}

pub(super) fn cn_state_tag(state: CnState) -> &'static str {
    match state {
        CnState::Pending => "pending",
        CnState::Approved => "approved",
        CnState::Disabled => "disabled",
    }
}

pub(super) fn digest_to_hex(digest: &[u8]) -> String {
    static HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}


// ── Migration (always-inline at extraction time; promoted here) ───────

pub(super) fn migration_by_id_key(id: Uuid) -> Vec<u8> {
    format!("migration/by_id/{id}").into_bytes()
}

pub(super) fn migration_active_key(instance_id: Uuid) -> Vec<u8> {
    format!("migration/active/{instance_id}").into_bytes()
}

pub(super) fn migration_by_instance_prefix(instance_id: Uuid) -> Vec<u8> {
    format!("migration/by_instance/{instance_id}/").into_bytes()
}

pub(super) fn migration_progress_key(migration_id: Uuid, seq: u64) -> Vec<u8> {
    format!("migration/progress/{migration_id}/{seq:016x}").into_bytes()
}

pub(super) fn migration_progress_prefix(migration_id: Uuid) -> Vec<u8> {
    format!("migration/progress/{migration_id}/").into_bytes()
}

#[cfg(test)]
mod tests {
    //! Lock the on-disk key shape of every builder so any
    //! schema-level change forces a deliberate test update (and
    //! triggers reviewers to consider the migration cost). When the
    //! Tuple Layer migration lands these expected strings flip to
    //! tuple-encoded bytes; until then they stay as `format!()` text.
    use super::*;

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).expect("test uuid")
    }

    #[test]
    fn identity_keys() {
        let s = uuid("11111111-1111-1111-1111-111111111111");
        let t = uuid("22222222-2222-2222-2222-222222222222");
        let u = uuid("33333333-3333-3333-3333-333333333333");
        assert_eq!(silo_by_id_key(s), b"silo/by_id/11111111-1111-1111-1111-111111111111");
        assert_eq!(silo_by_name_key("acme"), b"silo/by_name/acme");
        assert_eq!(tenant_by_id_key(t), b"tenant/by_id/22222222-2222-2222-2222-222222222222");
        assert_eq!(
            tenant_by_silo_name_key(s, "prod"),
            b"tenant/by_silo/11111111-1111-1111-1111-111111111111/prod",
        );
        assert_eq!(user_by_id_key(u), b"user/by_id/33333333-3333-3333-3333-333333333333");
        assert_eq!(user_by_name_key("alice"), b"user/by_name/alice");
        assert_eq!(user_prefix(), b"user/by_id/");
    }

    #[test]
    fn networking_keys() {
        let p = uuid("44444444-4444-4444-4444-444444444444");
        let v = uuid("55555555-5555-5555-5555-555555555555");
        let s = uuid("66666666-6666-6666-6666-666666666666");
        assert_eq!(
            vpc_by_id_key(v),
            b"vpc/by_id/55555555-5555-5555-5555-555555555555",
        );
        assert_eq!(
            vpc_in_project_key(p, v),
            b"vpc/in_project/44444444-4444-4444-4444-444444444444/55555555-5555-5555-5555-555555555555",
        );
        assert_eq!(vpc_by_vni_key(0xABCD), b"vpc/by_vni/0000abcd");
        assert_eq!(
            subnet_in_vpc_prefix(v),
            b"subnet/in_vpc/55555555-5555-5555-5555-555555555555/",
        );
        // The prefix length is load-bearing for slicing scanned keys.
        assert_eq!(subnet_in_vpc_prefix(v).len(), b"subnet/in_vpc/".len() + 36 + 1);
        assert_eq!(
            route_table_main_key(v),
            b"route_table/main/55555555-5555-5555-5555-555555555555",
        );
        // route by CIDR destination must keep the IpNetwork text form.
        let dest: IpNetwork = "10.0.0.0/8".parse().unwrap();
        assert_eq!(
            route_by_table_destination_key(v, dest),
            b"route/by_table/55555555-5555-5555-5555-555555555555/10.0.0.0/8",
        );
    }

    #[test]
    fn instance_indexes() {
        let i = uuid("77777777-7777-7777-7777-777777777777");
        let img = uuid("88888888-8888-8888-8888-888888888888");
        let cn = uuid("99999999-9999-9999-9999-999999999999");
        assert_eq!(
            instance_in_image_key(img, i),
            b"instance/in_image/88888888-8888-8888-8888-888888888888/77777777-7777-7777-7777-777777777777",
        );
        assert_eq!(
            instance_in_host_cn_prefix(cn),
            b"instance/in_host_cn/99999999-9999-9999-9999-999999999999/",
        );
    }

    #[test]
    fn job_pending_sorts_numerically() {
        // The hex-encoded sequence must lex-sort identically to numeric.
        let k1 = job_pending_key(1);
        let k2 = job_pending_key(2);
        let k16 = job_pending_key(0x10);
        let k_high = job_pending_key(u64::MAX);
        assert!(k1 < k2);
        assert!(k2 < k16);
        assert!(k16 < k_high);
        assert_eq!(k1, b"job/pending/0000000000000001");
        assert_eq!(k_high, b"job/pending/ffffffffffffffff");
    }

    #[test]
    fn migration_progress_sorts_numerically() {
        let m = uuid("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        let a = migration_progress_key(m, 1);
        let b = migration_progress_key(m, 0x100);
        let c = migration_progress_key(m, u64::MAX);
        assert!(a < b && b < c);
        assert_eq!(a, b"migration/progress/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa/0000000000000001");
    }

    #[test]
    fn federation_keys_hash_inputs() {
        // Two issuer URLs that differ only in trailing slash must hash
        // distinctly; this is the property the SHA-256 prefix provides.
        let t = uuid("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");
        let a = user_federation_key(t, "https://issuer.example.com", "subj");
        let b = user_federation_key(t, "https://issuer.example.com/", "subj");
        assert_ne!(a, b);
        // Issuer with a slash inside must not break the prefix shape.
        let c = user_federation_key(t, "https://issuer.example.com/realms/triton", "subj");
        assert!(c.starts_with(b"user/by_federation/bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb/"));
        // Hex is 64 chars + the prefix.
        assert_eq!(c.len(), b"user/by_federation/".len() + 36 + 1 + 64);
    }

    #[test]
    fn cn_state_tag_matches_serde_wire_format() {
        // These string tags are persisted in cn/by_state/<tag>/<uuid>.
        // Any drift between this mapping and CnState's
        // `#[serde(rename_all = "snake_case")]` silently corrupts the
        // by-state index.
        assert_eq!(cn_state_tag(CnState::Pending), "pending");
        assert_eq!(cn_state_tag(CnState::Approved), "approved");
        assert_eq!(cn_state_tag(CnState::Disabled), "disabled");
    }

    #[test]
    fn prefix_is_strict_prefix_of_key() {
        // Every `_prefix()` must be a strict byte prefix of any `_key()`
        // that lives in the same logical range. This is what makes
        // range scans correct.
        let v = uuid("cccccccc-cccc-cccc-cccc-cccccccccccc");
        let s = uuid("dddddddd-dddd-dddd-dddd-dddddddddddd");
        let pfx = subnet_in_vpc_prefix(v);
        let key = subnet_in_vpc_key(v, s);
        assert!(key.starts_with(&pfx), "{:?} not a prefix of {:?}", pfx, key);

        let rt = uuid("eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee");
        let r = uuid("ffffffff-ffff-ffff-ffff-ffffffffffff");
        assert!(route_in_table_key(rt, r).starts_with(&route_in_table_prefix(rt)));
    }
}
