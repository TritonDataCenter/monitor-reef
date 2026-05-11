// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Pure classifier for VMs reported by tritonagent's status collector.
//!
//! Status reports flow through [`classify_vm`] one VM at a time. The
//! function is sync and pure -- no Store, no FDB, no network --
//! because the calling status handler pre-fetches the relevant
//! instance lookup data inside the same FDB transaction that updates
//! `Cn.last_status`. That keeps classification atomic with the report
//! ingest and lets the test surface be a thin
//! `(report, context) -> outcome` table.
//!
//! See `STATUS.md` for the load-bearing decisions and the discovery
//! plan in `~/.claude/plans/one-of-the-better-giggly-zephyr.md` for
//! the design rationale.

use uuid::Uuid;

use tritond_auth::IdentityHmacKey;
use tritond_store::{Instance, LifecycleState, VmReport};

/// Outcome of classifying one VM in a CN status report.
///
/// `#[non_exhaustive]` per locked decision #14 -- adoption (Phase D)
/// will add `Adopting { from_smartos_uuid }` and a future fabric
/// integration may add `MetadataTampered`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Classification {
    /// Tritond manages this zone; the report's identity HMAC verifies,
    /// the carried `instance_id` is known, and the reporting CN is the
    /// instance's current host. The status handler updates the
    /// instance's runtime fields (lifecycle, memory, quota, etc.)
    /// from the report.
    Managed { instance_id: Uuid },
    /// The identity is valid but the reporting CN is *not* the
    /// instance's recorded host. Likely an out-of-band `vmadm
    /// send|recv` evacuation. Surfaces an alarm; the handler does
    /// not mutate either side.
    Orphan {
        instance_id: Uuid,
        expected_host: Option<Uuid>,
    },
    /// `tritond:*` metadata is present but it cannot be trusted.
    /// Either the HMAC didn't verify (forged or copied from another
    /// deployment), or the instance_id is unknown to this tritond.
    /// Quarantined: surfaced to fleet admin; no record mutation.
    StaleFingerprint { reason: StaleFingerprintReason },
    /// No `tritond:*` metadata yet, but the reported `smartos_uuid`
    /// matches a known `Instance.id` whose lifecycle is `Provisioning`
    /// and whose `host_cn_uuid` is the reporting CN. This is the
    /// race window between `vmadm create` starting and the agent
    /// having written the four `tritond:*` keys; the handler
    /// no-ops to avoid creating a phantom legacy record.
    MidProvision { instance_id: Uuid },
    /// Pre-existing legacy zone with no tritond identity. The handler
    /// upserts a `LegacyVm` record so the fleet-admin discovery
    /// surface can list it.
    Unmanaged,
}

/// Why a `tritond:*` identity tag was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StaleFingerprintReason {
    /// All four `tritond:*` keys present, but the HMAC over the
    /// `(instance_id, tenant_id, project_id)` triple did not verify
    /// against the deployment's identity HMAC key. Tampered metadata
    /// or a record copied from another deployment.
    HmacMismatch,
    /// Identity present and HMAC valid, but the carried
    /// `instance_id` is unknown to this deployment. Could be a stale
    /// record from a previous tritond install with a shared HMAC
    /// key, or a record we've already deleted.
    UnknownInstanceId,
}

/// Pure context for [`classify_vm`].
///
/// The status handler pre-fetches the instance lookup data inside its
/// FDB transaction so the classifier itself can be sync and pure.
/// `instance_lookup` returns `Some(&Instance)` for ids the store
/// knows about and `None` otherwise.
pub struct ClassifierContext<'a> {
    /// Server uuid of the CN whose status report we're classifying.
    pub reporting_cn_uuid: Uuid,
    /// Lookup an instance by its tritond `Instance.id`.
    pub instance_lookup: &'a dyn Fn(Uuid) -> Option<&'a Instance>,
    /// Per-deployment identity HMAC key used to sign and verify the
    /// `tritond:identity_hmac` tag.
    pub identity_hmac_key: &'a IdentityHmacKey,
}

/// Classify one [`VmReport`] against the store view captured in
/// [`ClassifierContext`].
#[must_use]
pub fn classify_vm(report: &VmReport, ctx: &ClassifierContext) -> Classification {
    match report.extract_managed_identity() {
        None => {
            // No `tritond:*` metadata. Two cases where we should
            // *not* treat this as a brand-new legacy zone:
            //
            // 1. Mid-provision race: known instance on this CN with
            //    lifecycle Provisioning, identity tags not yet
            //    visible to the collector. Suppress the LegacyVm
            //    upsert; the next status report will classify it
            //    Managed once the metadata lands.
            //
            // 2. Pre-existing managed zone: the smartos_uuid matches
            //    an Instance whose host_cn matches the reporter, but
            //    the zone was provisioned before identity stamping
            //    shipped (or operator cleared the metadata in-zone
            //    via `mdata-put`). Treat as Managed -- the
            //    Instance.id == zone uuid invariant is enough.
            //    Without this branch, every existing managed VM
            //    duplicates as a LegacyVm row.
            if let Some(inst) = (ctx.instance_lookup)(report.uuid)
                && inst.host_cn_uuid == Some(ctx.reporting_cn_uuid)
            {
                if matches!(inst.lifecycle, LifecycleState::Provisioning) {
                    return Classification::MidProvision {
                        instance_id: inst.id,
                    };
                }
                return Classification::Managed {
                    instance_id: inst.id,
                };
            }
            Classification::Unmanaged
        }
        Some(identity) => {
            // Verify HMAC first (cheap and rules out forged metadata
            // before we hit the store).
            if !ctx.identity_hmac_key.verify(
                identity.instance_id,
                identity.tenant_id,
                identity.project_id,
                &identity.identity_hmac,
            ) {
                return Classification::StaleFingerprint {
                    reason: StaleFingerprintReason::HmacMismatch,
                };
            }
            let Some(inst) = (ctx.instance_lookup)(identity.instance_id) else {
                return Classification::StaleFingerprint {
                    reason: StaleFingerprintReason::UnknownInstanceId,
                };
            };
            if inst.host_cn_uuid == Some(ctx.reporting_cn_uuid) {
                Classification::Managed {
                    instance_id: inst.id,
                }
            } else {
                Classification::Orphan {
                    instance_id: inst.id,
                    expected_host: inst.host_cn_uuid,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;
    use tritond_auth::IDENTITY_HMAC_KEY_BYTES;
    use tritond_store::{
        Instance, LifecycleState, TRITOND_METADATA_IDENTITY_HMAC, TRITOND_METADATA_INSTANCE_ID,
        TRITOND_METADATA_PROJECT_ID, TRITOND_METADATA_TENANT_ID, VmReport, VmState,
    };

    fn fixed_key() -> IdentityHmacKey {
        IdentityHmacKey::from_bytes([7u8; IDENTITY_HMAC_KEY_BYTES])
    }

    fn fresh_instance(host: Option<Uuid>, lifecycle: LifecycleState) -> Instance {
        Instance {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            name: "managed-zone".to_string(),
            description: String::new(),
            image_id: Uuid::new_v4(),
            primary_subnet_id: Uuid::new_v4(),
            ssh_key_ids: Vec::new(),
            cpu: 2,
            memory_bytes: 512 * 1024 * 1024,
            host_cn_uuid: host,
            lifecycle,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn report_for(uuid: Uuid, metadata: BTreeMap<String, String>) -> VmReport {
        VmReport {
            uuid,
            alias: None,
            brand: Some("joyent-minimal".to_string()),
            state: Some(VmState::Running),
            zone_state: Some("running".to_string()),
            max_physical_memory: Some(512 * 1024 * 1024),
            quota: Some(20 * 1024 * 1024 * 1024),
            cpu_cap: Some(200),
            owner_uuid: None,
            last_modified: None,
            internal_metadata: metadata,
            nics: Vec::new(),
        }
    }

    fn signed_metadata(
        key: &IdentityHmacKey,
        instance_id: Uuid,
        tenant_id: Uuid,
        project_id: Uuid,
    ) -> BTreeMap<String, String> {
        let hmac = key.sign(instance_id, tenant_id, project_id);
        let mut md = BTreeMap::new();
        md.insert(
            TRITOND_METADATA_INSTANCE_ID.to_string(),
            instance_id.to_string(),
        );
        md.insert(
            TRITOND_METADATA_TENANT_ID.to_string(),
            tenant_id.to_string(),
        );
        md.insert(
            TRITOND_METADATA_PROJECT_ID.to_string(),
            project_id.to_string(),
        );
        md.insert(TRITOND_METADATA_IDENTITY_HMAC.to_string(), hmac);
        md
    }

    fn classify_with(
        report: &VmReport,
        cn: Uuid,
        instance: Option<&Instance>,
        key: &IdentityHmacKey,
    ) -> Classification {
        let lookup = move |id: Uuid| -> Option<&Instance> { instance.filter(|i| i.id == id) };
        let ctx = ClassifierContext {
            reporting_cn_uuid: cn,
            instance_lookup: &lookup,
            identity_hmac_key: key,
        };
        classify_vm(report, &ctx)
    }

    #[test]
    fn unmanaged_zone_with_no_metadata_classifies_as_unmanaged() {
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let report = report_for(Uuid::new_v4(), BTreeMap::new());
        assert_eq!(
            classify_with(&report, cn, None, &key),
            Classification::Unmanaged
        );
    }

    #[test]
    fn managed_zone_on_correct_host_classifies_as_managed() {
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let inst = fresh_instance(Some(cn), LifecycleState::Running);
        let md = signed_metadata(&key, inst.id, inst.tenant_id, inst.project_id);
        let report = report_for(inst.id, md);
        assert_eq!(
            classify_with(&report, cn, Some(&inst), &key),
            Classification::Managed {
                instance_id: inst.id
            }
        );
    }

    #[test]
    fn managed_zone_on_other_host_classifies_as_orphan() {
        let key = fixed_key();
        let recorded_host = Uuid::new_v4();
        let reporting_cn = Uuid::new_v4();
        let inst = fresh_instance(Some(recorded_host), LifecycleState::Running);
        let md = signed_metadata(&key, inst.id, inst.tenant_id, inst.project_id);
        let report = report_for(inst.id, md);
        let classification = classify_with(&report, reporting_cn, Some(&inst), &key);
        assert_eq!(
            classification,
            Classification::Orphan {
                instance_id: inst.id,
                expected_host: Some(recorded_host),
            }
        );
    }

    #[test]
    fn metadata_with_bad_hmac_classifies_as_stale_fingerprint() {
        let key_a = fixed_key();
        let key_b = IdentityHmacKey::from_bytes([0x99u8; IDENTITY_HMAC_KEY_BYTES]);
        let cn = Uuid::new_v4();
        let inst = fresh_instance(Some(cn), LifecycleState::Running);
        // Sign with the wrong key.
        let md = signed_metadata(&key_b, inst.id, inst.tenant_id, inst.project_id);
        let report = report_for(inst.id, md);
        // Verify with key_a (the deployment key).
        assert_eq!(
            classify_with(&report, cn, Some(&inst), &key_a),
            Classification::StaleFingerprint {
                reason: StaleFingerprintReason::HmacMismatch,
            }
        );
    }

    #[test]
    fn metadata_pointing_at_unknown_instance_classifies_as_stale_fingerprint() {
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let phantom_id = Uuid::new_v4();
        let phantom_tenant = Uuid::new_v4();
        let phantom_project = Uuid::new_v4();
        // HMAC verifies (signed with the deployment key) but the
        // store doesn't know the instance_id.
        let md = signed_metadata(&key, phantom_id, phantom_tenant, phantom_project);
        let report = report_for(phantom_id, md);
        assert_eq!(
            classify_with(&report, cn, None, &key),
            Classification::StaleFingerprint {
                reason: StaleFingerprintReason::UnknownInstanceId,
            }
        );
    }

    #[test]
    fn mid_provision_race_window_classifies_as_mid_provision_not_unmanaged() {
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let inst = fresh_instance(Some(cn), LifecycleState::Provisioning);
        // Metadata not yet visible: empty internal_metadata, but the
        // smartos_uuid matches a known Instance whose lifecycle is
        // Provisioning on this CN.
        let report = report_for(inst.id, BTreeMap::new());
        assert_eq!(
            classify_with(&report, cn, Some(&inst), &key),
            Classification::MidProvision {
                instance_id: inst.id
            }
        );
    }

    #[test]
    fn missing_metadata_with_known_instance_on_other_cn_is_unmanaged_not_mid_provision() {
        // Defensive: don't allow the mid-provision short-circuit to
        // mask an out-of-band move (the zone's smartos_uuid happens
        // to match an unrelated instance whose host_cn doesn't match
        // the reporter).
        let key = fixed_key();
        let recorded_host = Uuid::new_v4();
        let reporting_cn = Uuid::new_v4();
        let inst = fresh_instance(Some(recorded_host), LifecycleState::Provisioning);
        let report = report_for(inst.id, BTreeMap::new());
        assert_eq!(
            classify_with(&report, reporting_cn, Some(&inst), &key),
            Classification::Unmanaged,
        );
    }

    #[test]
    fn missing_metadata_with_known_instance_in_running_state_classifies_as_managed() {
        // A Running instance whose smartos_uuid matches a known
        // Instance on this CN, but is missing the tritond:* metadata
        // (provisioned before identity stamping shipped, OR operator
        // cleared the tags in-zone via `mdata-put`), classifies
        // Managed -- the Instance.id == zone uuid invariant is
        // enough to claim ownership without the metadata. Without
        // this branch every pre-existing managed VM would duplicate
        // as a LegacyVm row.
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let inst = fresh_instance(Some(cn), LifecycleState::Running);
        let report = report_for(inst.id, BTreeMap::new());
        assert_eq!(
            classify_with(&report, cn, Some(&inst), &key),
            Classification::Managed {
                instance_id: inst.id
            },
        );
    }

    #[test]
    fn partial_tritond_metadata_classifies_as_unmanaged() {
        // Three of the four tritond:* keys present and the fourth
        // missing -- the extractor returns None, so the zone falls
        // through to Unmanaged (not StaleFingerprint). Rationale: a
        // half-stamped zone is more likely to be a partial in-zone
        // edit than an actual managed zone we want to alarm on.
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let mut md = BTreeMap::new();
        md.insert(
            TRITOND_METADATA_INSTANCE_ID.to_string(),
            Uuid::new_v4().to_string(),
        );
        md.insert(
            TRITOND_METADATA_TENANT_ID.to_string(),
            Uuid::new_v4().to_string(),
        );
        // omit project_id and identity_hmac
        let report = report_for(Uuid::new_v4(), md);
        assert_eq!(
            classify_with(&report, cn, None, &key),
            Classification::Unmanaged,
        );
    }

    #[test]
    fn empty_hmac_string_classifies_as_unmanaged_not_stale_fingerprint() {
        // Operator runs `mdata-put tritond:identity_hmac ''` to clear
        // the tag in-zone. Our extractor treats empty hmac as "no
        // identity"; the zone falls to Unmanaged. Tampered records
        // (non-empty bad hmac) DO get StaleFingerprint.
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let mut md = BTreeMap::new();
        md.insert(
            TRITOND_METADATA_INSTANCE_ID.to_string(),
            Uuid::new_v4().to_string(),
        );
        md.insert(
            TRITOND_METADATA_TENANT_ID.to_string(),
            Uuid::new_v4().to_string(),
        );
        md.insert(
            TRITOND_METADATA_PROJECT_ID.to_string(),
            Uuid::new_v4().to_string(),
        );
        md.insert(TRITOND_METADATA_IDENTITY_HMAC.to_string(), String::new());
        let report = report_for(Uuid::new_v4(), md);
        assert_eq!(
            classify_with(&report, cn, None, &key),
            Classification::Unmanaged,
        );
    }

    #[test]
    fn malformed_uuid_in_metadata_classifies_as_unmanaged() {
        // extract_managed_identity returns None on parse failure;
        // classifier falls through to Unmanaged. Tampered string
        // values that are not UUIDs are not worth alarming on -- it's
        // the same observable shape as an in-zone clobber.
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let mut md = BTreeMap::new();
        md.insert(
            TRITOND_METADATA_INSTANCE_ID.to_string(),
            "not-a-uuid".to_string(),
        );
        md.insert(
            TRITOND_METADATA_TENANT_ID.to_string(),
            Uuid::new_v4().to_string(),
        );
        md.insert(
            TRITOND_METADATA_PROJECT_ID.to_string(),
            Uuid::new_v4().to_string(),
        );
        md.insert(
            TRITOND_METADATA_IDENTITY_HMAC.to_string(),
            "deadbeef".to_string(),
        );
        let report = report_for(Uuid::new_v4(), md);
        assert_eq!(
            classify_with(&report, cn, None, &key),
            Classification::Unmanaged,
        );
    }

    #[test]
    fn managed_zone_state_does_not_affect_classification() {
        // Same identity, same host -- whether the zone is Running,
        // Stopped, or Failed, classification is Managed. The
        // reconciliation rules (which fields to update) are a
        // separate concern.
        let key = fixed_key();
        let cn = Uuid::new_v4();
        for state in [
            LifecycleState::Running,
            LifecycleState::Stopped,
            LifecycleState::Pending,
            LifecycleState::Stopping,
            LifecycleState::Failed {
                reason: "boot fail".to_string(),
            },
        ] {
            let inst = fresh_instance(Some(cn), state.clone());
            let md = signed_metadata(&key, inst.id, inst.tenant_id, inst.project_id);
            let report = report_for(inst.id, md);
            assert_eq!(
                classify_with(&report, cn, Some(&inst), &key),
                Classification::Managed {
                    instance_id: inst.id
                },
                "state {state:?} should still classify Managed",
            );
        }
    }

    #[test]
    fn instance_with_no_host_cn_classifies_as_orphan_not_managed() {
        // An instance whose host_cn_uuid is None (e.g. placement
        // hasn't run yet) reporting on a CN cannot be Managed --
        // we don't know if this CN is the intended host. Surface as
        // Orphan so an operator can investigate.
        let key = fixed_key();
        let cn = Uuid::new_v4();
        let inst = fresh_instance(None, LifecycleState::Pending);
        let md = signed_metadata(&key, inst.id, inst.tenant_id, inst.project_id);
        let report = report_for(inst.id, md);
        assert_eq!(
            classify_with(&report, cn, Some(&inst), &key),
            Classification::Orphan {
                instance_id: inst.id,
                expected_host: None,
            }
        );
    }
}
