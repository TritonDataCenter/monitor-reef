// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `floating-ip-allocate` / `floating-ip-attach` /
//! `floating-ip-detach` sagas.
//!
//! Single-store-call operations on the surface — saga-shape is
//! light. Their value is the resource-reference index entries
//! (tenant / project / fip / nic) so the per-FIP and per-VM saga
//! views can surface them, and the standard saga unwind story
//! (record the original state, restore on failure).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, DagBuilder, Node, ResourceRef,
    ResourceScope, SagaDag, SagaError, SagaName, SagaResult, TritondSagaType,
};
use tritond_store::{FloatingIp, NewFloatingIp};
use uuid::Uuid;

use super::common::{ACTION_TIMEOUT_STORE, Ctx, fence_check, no_op_undo, store_err_to_action_err};

pub const SAGA_NAME_ALLOCATE: &str = "floating-ip-allocate";
pub const SAGA_NAME_ATTACH: &str = "floating-ip-attach";
pub const SAGA_NAME_DETACH: &str = "floating-ip-detach";
pub const SAGA_VERSION: u32 = 1;

pub fn register(reg: &mut ActionRegistry) {
    reg.register(ActionFunc::new_action(
        "floating_ip.allocate",
        allocate,
        allocate_undo,
    ));
    reg.register(ActionFunc::new_action(
        "floating_ip.attach",
        attach,
        attach_undo,
    ));
    reg.register(ActionFunc::new_action(
        "floating_ip.detach",
        detach,
        detach_undo,
    ));
}

// ===============================================================
// floating-ip-allocate
// ===============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingIpAllocateParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub request: NewFloatingIp,
}

pub fn build_allocate_dag(params: &FloatingIpAllocateParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME_ALLOCATE));
    b.append(Node::action(
        "fip",
        "allocate",
        &*ActionFunc::new_action("floating_ip.allocate", allocate, allocate_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_allocate_references(params: &FloatingIpAllocateParams) -> Vec<ResourceRef> {
    vec![
        ResourceRef::new(ResourceScope::Tenant, params.tenant_id),
        ResourceRef::new(ResourceScope::Project, params.project_id),
    ]
}

async fn allocate(ctx: Ctx) -> Result<FloatingIp, ActionError> {
    crate::sagas::with_action_timeout("floating_ip.allocate", ACTION_TIMEOUT_STORE, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: FloatingIpAllocateParams = ctx.saga_params()?;
        store
            .create_floating_ip(params.tenant_id, params.project_id, params.request)
            .await
            .map_err(store_err_to_action_err)
    })
    .await
}

async fn allocate_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let Ok(fip) = ctx.lookup::<FloatingIp>("fip") else {
        return Ok(());
    };
    match store.delete_floating_ip(fip.id).await {
        Ok(()) | Err(tritond_store::StoreError::NotFound) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("delete_floating_ip during undo: {e}")),
    }
}

// ===============================================================
// floating-ip-attach
// ===============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingIpAttachParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub fip_id: Uuid,
    pub target_nic_id: Uuid,
    /// Captured by the handler before the saga runs so the undo
    /// can restore the prior binding. `None` = the FIP was
    /// floating; undo detaches.
    #[serde(default)]
    pub prior_nic_id: Option<Uuid>,
    /// The instance the target NIC belongs to (resolved by the
    /// handler from the NIC record). Goes into the saga's
    /// reference index so the per-instance saga view picks the
    /// attach up.
    #[serde(default)]
    pub target_instance_id: Option<Uuid>,
}

pub fn build_attach_dag(params: &FloatingIpAttachParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME_ATTACH));
    b.append(Node::action(
        "attached",
        "attach",
        &*ActionFunc::new_action("floating_ip.attach", attach, attach_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_attach_references(params: &FloatingIpAttachParams) -> Vec<ResourceRef> {
    let mut out = vec![
        ResourceRef::new(ResourceScope::Tenant, params.tenant_id),
        ResourceRef::new(ResourceScope::Project, params.project_id),
        ResourceRef::new(ResourceScope::FloatingIp, params.fip_id),
        ResourceRef::new(ResourceScope::Nic, params.target_nic_id),
    ];
    if let Some(iid) = params.target_instance_id {
        out.push(ResourceRef::new(ResourceScope::Instance, iid));
    }
    out
}

async fn attach(ctx: Ctx) -> Result<FloatingIp, ActionError> {
    crate::sagas::with_action_timeout("floating_ip.attach", ACTION_TIMEOUT_STORE, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: FloatingIpAttachParams = ctx.saga_params()?;
        // Placement validation (C-3): a FIP that carries an
        // `external_nic_tag` may only be claimed on a CN that
        // advertises that nic_tag. Resolve NIC -> instance ->
        // host_cn, then assert membership in the CN's published
        // inventory. Fail closed if the tag does not resolve.
        validate_nic_tag_placement(&*store, params.fip_id, params.target_nic_id)
            .await
            .map_err(store_err_to_action_err)?;
        store
            .attach_floating_ip(params.fip_id, params.target_nic_id)
            .await
            .map_err(store_err_to_action_err)
    })
    .await
}

/// Assert that, if the floating IP carries an `external_nic_tag`, the
/// CN hosting the target NIC's instance advertises that tag. A FIP
/// with no `external_nic_tag` (legacy `family`-allocated) skips the
/// check. Fail-closed: a tag that resolves to no provided entry — or a
/// NIC / instance / host_cn / inventory that does not resolve — is a
/// [`StoreError::NicTagNotProvided`] (D10f).
async fn validate_nic_tag_placement(
    store: &dyn tritond_store::Store,
    fip_id: Uuid,
    target_nic_id: Uuid,
) -> Result<(), tritond_store::StoreError> {
    let fip = store.get_floating_ip(fip_id).await?;
    let Some(nic_tag) = fip.external_nic_tag else {
        // Legacy family path: no external nic_tag to enforce.
        return Ok(());
    };
    let nic = store.get_nic(target_nic_id).await?;
    let instance = store.get_instance(nic.instance_id).await?;
    let host_cn = instance
        .host_cn_uuid
        .ok_or(tritond_store::StoreError::NicTagNotProvided {
            cn: Uuid::nil(),
            nic_tag,
        })?;
    let provided = store
        .get_cn_nic_tags(host_cn)
        .await?
        .map(|inv| inv.provides.iter().any(|p| p.nic_tag == nic_tag))
        .unwrap_or(false);
    if provided {
        Ok(())
    } else {
        Err(tritond_store::StoreError::NicTagNotProvided {
            cn: host_cn,
            nic_tag,
        })
    }
}

async fn attach_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let log = ctx.user_data().log().clone();
    let Ok(params) = ctx.saga_params::<FloatingIpAttachParams>() else {
        return Ok(());
    };
    match params.prior_nic_id {
        Some(prior) => match store.attach_floating_ip(params.fip_id, prior).await {
            Ok(_) => Ok(()),
            Err(tritond_store::StoreError::NotFound) => Ok(()),
            Err(e) => {
                slog::warn!(
                    log,
                    "floating-ip-attach undo: restore-prior-binding failed";
                    "fip_id" => %params.fip_id,
                    "prior_nic_id" => %prior,
                    "error" => %e,
                );
                Ok(())
            }
        },
        None => match store.detach_floating_ip(params.fip_id).await {
            Ok(_) | Err(tritond_store::StoreError::NotFound) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("detach during attach-undo: {e}")),
        },
    }
}

// ===============================================================
// floating-ip-detach
// ===============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingIpDetachParams {
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub fip_id: Uuid,
    /// Captured by the handler before the saga runs so the undo
    /// can re-attach. `None` means the FIP was already floating
    /// — detach is a no-op and the undo has nothing to restore.
    #[serde(default)]
    pub prior_nic_id: Option<Uuid>,
    #[serde(default)]
    pub prior_instance_id: Option<Uuid>,
}

pub fn build_detach_dag(params: &FloatingIpDetachParams) -> SagaResult<Arc<SagaDag>> {
    let mut b = DagBuilder::new(SagaName::new(SAGA_NAME_DETACH));
    b.append(Node::action(
        "detached",
        "detach",
        &*ActionFunc::new_action("floating_ip.detach", detach, detach_undo),
    ));
    let dag = b
        .build()
        .map_err(|e| SagaError::Backend(format!("dag build: {e}")))?;
    let params_json = serde_json::to_value(params)
        .map_err(|e| SagaError::Backend(format!("params serialize: {e}")))?;
    Ok(Arc::new(SagaDag::new(dag, params_json)))
}

pub fn build_detach_references(params: &FloatingIpDetachParams) -> Vec<ResourceRef> {
    let mut out = vec![
        ResourceRef::new(ResourceScope::Tenant, params.tenant_id),
        ResourceRef::new(ResourceScope::Project, params.project_id),
        ResourceRef::new(ResourceScope::FloatingIp, params.fip_id),
    ];
    if let Some(nic) = params.prior_nic_id {
        out.push(ResourceRef::new(ResourceScope::Nic, nic));
    }
    if let Some(iid) = params.prior_instance_id {
        out.push(ResourceRef::new(ResourceScope::Instance, iid));
    }
    out
}

async fn detach(ctx: Ctx) -> Result<FloatingIp, ActionError> {
    crate::sagas::with_action_timeout("floating_ip.detach", ACTION_TIMEOUT_STORE, async move {
        let user_ctx = ctx.user_data();
        fence_check(user_ctx).await?;
        let store = user_ctx.store().clone();
        let params: FloatingIpDetachParams = ctx.saga_params()?;
        store
            .detach_floating_ip(params.fip_id)
            .await
            .map_err(store_err_to_action_err)
    })
    .await
}

async fn detach_undo(ctx: Ctx) -> Result<(), anyhow::Error> {
    let store = ctx.user_data().store().clone();
    let log = ctx.user_data().log().clone();
    let Ok(params) = ctx.saga_params::<FloatingIpDetachParams>() else {
        return Ok(());
    };
    let Some(prior) = params.prior_nic_id else {
        return Ok(());
    };
    match store.attach_floating_ip(params.fip_id, prior).await {
        Ok(_) => Ok(()),
        Err(tritond_store::StoreError::NotFound) => Ok(()),
        Err(e) => {
            slog::warn!(
                log,
                "floating-ip-detach undo: restore-prior-binding failed";
                "fip_id" => %params.fip_id,
                "prior_nic_id" => %prior,
                "error" => %e,
            );
            Ok(())
        }
    }
}

// no_op stand-ins so the un-used import lints stay quiet in builds
// that compile only allocate / attach / detach in isolation.
#[allow(dead_code)]
async fn _unused(_ctx: Ctx) -> Result<(), anyhow::Error> {
    no_op_undo(_ctx).await
}

pub fn decode_store_error_kind(source: &serde_json::Value) -> Option<&'static str> {
    super::common::decode_store_error_kind(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tritond_store::{
        AddressFamily, CnNicTagInventory, MemStore, NewExternalSubnet, NewImage, NewInstance,
        NewNicTag, NewProject, NewSilo, NewSshKey, NewSubnet, NewVpc, Store,
    };

    /// Build an instance with a NIC on a host CN, plus a
    /// network-allocated FIP that carries an `external_nic_tag`.
    /// Returns (store, fip_id, nic_id, host_cn, external_nic_tag).
    async fn fixture() -> (MemStore, Uuid, Uuid, Uuid, Uuid) {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "s".into(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;
        let project = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "p".into(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let project_id = project.id;
        let vpc = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "v".into(),
                    description: None,
                    ipv4_block: Some("10.0.0.0/16".parse().unwrap()),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let subnet = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "primary".into(),
                    description: None,
                    ipv4_block: Some("10.0.1.0/24".parse().unwrap()),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let image = store
            .create_image_silo(
                silo.id,
                NewImage {
                    name: "img".into(),
                    description: None,
                    os: "linux".into(),
                    version: "1".into(),
                    size_bytes: 1_000_000,
                    sha256: "a".repeat(64),
                    source_url: Some("mantafs://i".into()),
                    id: None,
                    compatibility: None,
                },
            )
            .await
            .unwrap();
        let ssh = store
            .create_ssh_key_silo(
                silo.id,
                NewSshKey {
                    name: "k".into(),
                    description: None,
                    public_key: "ssh-ed25519 AAAA".into(),
                },
                "SHA256:fixture".into(),
            )
            .await
            .unwrap();
        let created = store
            .create_instance(
                tenant_id,
                project_id,
                NewInstance {
                    name: "web".into(),
                    description: None,
                    image_id: image.id,
                    primary_subnet_id: subnet.id,
                    ssh_key_ids: vec![ssh.id],
                    cpu: 1,
                    memory_bytes: 1024 * 1024 * 1024,
                    mac: None,
                    extra_nics: Vec::new(),
                },
            )
            .await
            .unwrap();
        let host_cn = Uuid::new_v4();
        store
            .set_instance_host_cn(created.instance.id, Some(host_cn))
            .await
            .unwrap();

        let tag = store
            .create_nic_tag(NewNicTag {
                name: "external".into(),
                description: None,
                mtu: 1500,
            })
            .await
            .unwrap();
        let ext = store
            .create_external_subnet(NewExternalSubnet {
                name: "pub".into(),
                description: None,
                ipv4_block: Some("192.0.2.0/24".parse().unwrap()),
                ipv6_block: None,
                nic_tag: tag.id,
                vlan_id: Some(100),
                provision_start_ipv4: Some(Ipv4Addr::new(192, 0, 2, 10)),
                provision_end_ipv4: Some(Ipv4Addr::new(192, 0, 2, 12)),
                provision_start_ipv6: None,
                provision_end_ipv6: None,
                owner_silos: Vec::new(),
            })
            .await
            .unwrap();
        let fip = store
            .create_floating_ip(
                tenant_id,
                project_id,
                NewFloatingIp {
                    name: "fip".into(),
                    description: None,
                    family: None,
                    network_id: Some(ext.id),
                    pool_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(fip.external_nic_tag, Some(tag.id));

        (store, fip.id, created.nics[0].id, host_cn, tag.id)
    }

    #[tokio::test]
    async fn placement_ok_when_cn_provides_tag() {
        let (store, fip_id, nic_id, host_cn, tag_id) = fixture().await;
        store
            .publish_cn_nic_tags(CnNicTagInventory {
                cn: host_cn,
                provides: vec![tritond_store::NicTagProvision {
                    nic_tag: tag_id,
                    physical_nic: "igb0".into(),
                    vlan_id: 100,
                    mtu: 1500,
                }],
                published_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        validate_nic_tag_placement(&store, fip_id, nic_id)
            .await
            .expect("CN advertises the tag");
    }

    #[tokio::test]
    async fn placement_rejected_when_cn_lacks_tag() {
        // Host CN publishes an inventory that does NOT include the
        // FIP's external_nic_tag.
        let (store, fip_id, nic_id, host_cn, _tag_id) = fixture().await;
        store
            .publish_cn_nic_tags(CnNicTagInventory {
                cn: host_cn,
                provides: vec![tritond_store::NicTagProvision {
                    nic_tag: Uuid::new_v4(),
                    physical_nic: "igb1".into(),
                    vlan_id: 200,
                    mtu: 1500,
                }],
                published_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        let err = validate_nic_tag_placement(&store, fip_id, nic_id)
            .await
            .expect_err("CN does not advertise the tag");
        assert!(matches!(
            err,
            tritond_store::StoreError::NicTagNotProvided { .. }
        ));
    }

    #[tokio::test]
    async fn placement_fails_closed_when_inventory_absent() {
        // No CnNicTagInventory at all => fail closed.
        let (store, fip_id, nic_id, _host_cn, _tag_id) = fixture().await;
        let err = validate_nic_tag_placement(&store, fip_id, nic_id)
            .await
            .expect_err("absent inventory must fail closed");
        assert!(matches!(
            err,
            tritond_store::StoreError::NicTagNotProvided { .. }
        ));
    }

    #[tokio::test]
    async fn placement_skips_legacy_fip_without_tag() {
        // A legacy family-allocated FIP carries no external_nic_tag,
        // so placement validation is a no-op (no inventory needed).
        let (store, _fip_id, nic_id, _host_cn, _tag_id) = fixture().await;
        let nic = store.get_nic(nic_id).await.unwrap();
        let instance = store.get_instance(nic.instance_id).await.unwrap();
        let legacy = store
            .create_floating_ip(
                instance.tenant_id,
                instance.project_id,
                NewFloatingIp {
                    name: "legacy".into(),
                    description: None,
                    family: Some(AddressFamily::V4),
                    network_id: None,
                    pool_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(legacy.external_nic_tag, None);
        validate_nic_tag_placement(&store, legacy.id, nic_id)
            .await
            .expect("legacy fip skips nic_tag placement");
    }
}
