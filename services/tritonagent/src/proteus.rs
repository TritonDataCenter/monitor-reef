// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Tritonagent adapter around the Proteus userspace client.
//!
//! The agent owns lifecycle ordering, but the driver remains the source
//! of truth for realized state. This module keeps those calls small and
//! deterministic so the job loop can create/apply/start ports before
//! `vmadm create`, and pause/delete them during cleanup.

use std::net::IpAddr;

use anyhow::{Context, Result, anyhow, bail};
use proteus_api::blueprint::{BlueprintApplyStatus, PortBlueprint, PortSummary};
use proteus_api::dump::GenerationStatus;
use proteus_api::error::ProteusError;
use proteus_api::ids::PortId;
use proteus_api::peer::PeerAddrFamily;
use proteus_api::requests::{CreatePortRequest, EnsureExternalLinkRequest};
use proteus_api::floating_ip::InvalidateFipEntryRequest;
use proteus_ioctl::{Client, Error as IoctlError, Transport};

/// Build the SmartOS datalink name for a Proteus port.
///
/// `proteusadm` uses the same contract: take the low 32 bits of the
/// port UUID, render them as decimal with no leading zeroes, and prefix
/// the value with `proteus`. SmartOS-live accepts that name as the
/// dynamic `nic_tag` parent for M1 bhyve NICs.
pub fn link_name_for_port(port_id: PortId) -> String {
    let suffix = port_id.0.as_u128() as u32;
    format!("proteus{suffix}")
}

/// Realized status collected after a lifecycle transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProteusPortStatus {
    pub summary: PortSummary,
    pub generation: GenerationStatus,
}

/// Thin typed wrapper over a Proteus userspace transport.
pub struct ProteusClient<T: Transport> {
    client: Client<T>,
}

impl<T: Transport> ProteusClient<T> {
    /// Build an adapter from a real kernel transport or the in-process fake.
    pub fn new(transport: T) -> Self {
        Self {
            client: Client::new(transport),
        }
    }

    /// Reserve the port slot described by `blueprint`.
    ///
    /// Create is idempotent for agent retry: if the driver reports an error
    /// but `GetPortSummary` can verify the same port/network already exists,
    /// the adapter treats the create as successful.
    pub fn create_port(&self, blueprint: &PortBlueprint, linkid: Option<u32>) -> Result<()> {
        let req = CreatePortRequest {
            port_id: blueprint.port_id,
            network_id: blueprint.network_id,
            link: blueprint.link.clone(),
            limits: blueprint.limits.clone(),
            linkid,
        };

        match self.client.create_port(&req) {
            Ok(()) => Ok(()),
            Err(err) if self.port_matches(blueprint)? => Ok(()),
            Err(err) => Err(err).with_context(|| {
                format!(
                    "create Proteus port {} for network {:?}",
                    blueprint.port_id, blueprint.network_id,
                )
            }),
        }
    }

    /// Apply a desired port blueprint.
    ///
    /// Retrying the same generation can return `StaleGeneration`; that is
    /// success only when the driver reports the desired generation already
    /// applied.
    pub fn apply_blueprint(&self, blueprint: &PortBlueprint) -> Result<()> {
        match self.client.apply_blueprint(blueprint) {
            Ok(()) => Ok(()),
            Err(IoctlError::Driver(ProteusError::StaleGeneration { .. })) => {
                let status = self.generation_status(blueprint.port_id)?;
                if status.applied_generation >= blueprint.generation
                    && status.apply_status == BlueprintApplyStatus::Applied
                {
                    Ok(())
                } else {
                    bail!(
                        "Proteus port {} has stale generation {:?}; desired {:?}",
                        blueprint.port_id,
                        status.applied_generation,
                        blueprint.generation,
                    )
                }
            }
            Err(err) => Err(err)
                .with_context(|| format!("apply Proteus blueprint for port {}", blueprint.port_id)),
        }
    }

    /// Start packet processing for a port.
    pub fn start_port(&self, port_id: PortId) -> Result<()> {
        self.client
            .start_port(port_id)
            .with_context(|| format!("start Proteus port {port_id}"))
    }

    /// Pause packet processing for a port while preserving state.
    pub fn pause_port(&self, port_id: PortId) -> Result<()> {
        self.client
            .pause_port(port_id)
            .with_context(|| format!("pause Proteus port {port_id}"))
    }

    /// Delete a port slot.
    ///
    /// `UnknownPort` is success for cleanup because delete jobs are
    /// idempotent and may race with a previous failed provision cleanup.
    pub fn delete_port(&self, port_id: PortId) -> Result<()> {
        match self.client.delete_port(port_id) {
            Ok(()) | Err(IoctlError::Driver(ProteusError::UnknownPort)) => Ok(()),
            Err(err) => Err(err).with_context(|| format!("delete Proteus port {port_id}")),
        }
    }

    /// Return the driver's realized status for a port.
    pub fn dump_status(&self, port_id: PortId) -> Result<ProteusPortStatus> {
        let summary = self
            .client
            .get_port_summary(port_id)
            .with_context(|| format!("get Proteus summary for port {port_id}"))?;
        let generation = self.generation_status(port_id)?;
        Ok(ProteusPortStatus {
            summary,
            generation,
        })
    }

    /// Create, apply, and start a port, then return realized status.
    pub fn ensure_started(
        &self,
        blueprint: &PortBlueprint,
        linkid: Option<u32>,
    ) -> Result<ProteusPortStatus> {
        self.create_port(blueprint, linkid)?;
        self.apply_blueprint(blueprint)?;
        self.assert_generation_applied(blueprint)?;
        self.start_port(blueprint.port_id)?;
        self.dump_status(blueprint.port_id)
    }

    /// Best-effort cleanup for delete and failed-provision unwinds.
    pub fn cleanup_port(&self, port_id: PortId) -> Result<()> {
        match self.pause_port(port_id) {
            Ok(()) => {}
            Err(err) => {
                if self.is_unknown_port(port_id)? {
                    return Ok(());
                }
                return Err(err);
            }
        }
        self.delete_port(port_id)
    }

    /// Idempotently register the per-CN external datalink the inbound
    /// FIP siphon attaches to (C-4b). `linkid` is resolved by the
    /// caller via libdladm; `link_name` is carried for diagnostics.
    /// Named `_with_id` to distinguish it from the agent's
    /// `ProteusLifecycle::ensure_external_link` trait method (which
    /// resolves the linkid first and delegates here).
    pub fn ensure_external_link_with_id(&self, linkid: u32, link_name: &str) -> Result<()> {
        self.client
            .ensure_external_link(&EnsureExternalLinkRequest {
                linkid,
                link_name: link_name.to_string(),
                // source_mac / gateway_mac default to all-zero here.
                // Zeros are SAFE: opte_tx_to_external_link fails closed
                // on an all-zero MAC (it drops rather than emit a frame
                // with a bogus L2 src/dst). The REAL source_mac (the
                // external NIC's own MAC) and gateway_mac (the upstream
                // router MAC from neighbor discovery) must be resolved
                // control-plane-side and seeded at M5 (a C-4b/M5
                // followup) before the outbound ExternalTx path emits.
                ..Default::default()
            })
            .with_context(|| format!("ensure Proteus external link {link_name} (linkid {linkid})"))
    }

    /// Invalidate one hosted-FIP entry by address (C-4b `FipRelease`,
    /// step 1). Idempotent: a missing entry is a no-op at the kmod, so
    /// withdrawing a FIP that was never installed (or already removed)
    /// succeeds. Called BEFORE the alias/blueprint teardown so the
    /// inbound classifier stops delivering to the guest first.
    pub fn invalidate_hosted_fip(&self, addr: IpAddr) -> Result<()> {
        let (family, addr) = fip_addr_to_wire(addr);
        self.client
            .invalidate_fip_entry(&InvalidateFipEntryRequest { family, addr })
            .map(|_ack| ())
            .with_context(|| "invalidate Proteus hosted-FIP entry")
    }
}

/// Encode a FIP address into the proteus wire `(family, [u8; 16])`
/// shape used by `AddFipEntryRequest` / `InvalidateFipEntryRequest`. v4
/// occupies the first 4 bytes (rest zero); v6 fills all 16.
fn fip_addr_to_wire(addr: IpAddr) -> (PeerAddrFamily, [u8; 16]) {
    let mut bytes = [0u8; 16];
    match addr {
        IpAddr::V4(v4) => {
            bytes[..4].copy_from_slice(&v4.octets());
            (PeerAddrFamily::V4, bytes)
        }
        IpAddr::V6(v6) => {
            bytes.copy_from_slice(&v6.octets());
            (PeerAddrFamily::V6, bytes)
        }
    }
}

impl<T: Transport> ProteusClient<T> {
    fn generation_status(&self, port_id: PortId) -> Result<GenerationStatus> {
        self.client
            .generation_status(port_id)
            .with_context(|| format!("get Proteus generation status for port {port_id}"))
    }

    pub(crate) fn assert_generation_applied(&self, blueprint: &PortBlueprint) -> Result<()> {
        let status = self.generation_status(blueprint.port_id)?;
        if status.applied_generation >= blueprint.generation
            && status.apply_status == BlueprintApplyStatus::Applied
        {
            return Ok(());
        }

        Err(anyhow!(
            "Proteus port {} generation {:?} is not applied; desired {:?}, status {:?}: {}",
            blueprint.port_id,
            status.applied_generation,
            blueprint.generation,
            status.apply_status,
            status.degradation_reason,
        ))
    }

    fn port_matches(&self, blueprint: &PortBlueprint) -> Result<bool> {
        match self.client.get_port_summary(blueprint.port_id) {
            Ok(summary) => Ok(summary.network_id == blueprint.network_id),
            Err(IoctlError::Driver(ProteusError::UnknownPort)) => Ok(false),
            Err(err) => Err(err)
                .with_context(|| format!("verify existing Proteus port {}", blueprint.port_id)),
        }
    }

    fn is_unknown_port(&self, port_id: PortId) -> Result<bool> {
        match self.client.get_port_summary(port_id) {
            Ok(_) => Ok(false),
            Err(IoctlError::Driver(ProteusError::UnknownPort)) => Ok(true),
            Err(err) => Err(err)
                .with_context(|| format!("verify missing Proteus port {port_id} during cleanup")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proteus_api::blueprint::{
        ClientLinkConfig, PORT_BLUEPRINT_SCHEMA_V0, PluginConfigBytes, PortLimits, PortState,
    };
    use proteus_api::ids::{Generation, NetworkId};
    use proteus_ioctl::FakeTransport;
    use uuid::Uuid;

    #[test]
    fn link_name_for_port_matches_proteusadm_contract() {
        let port_id = PortId(Uuid::parse_str("00000000-0000-4000-8000-00000000c0e1").unwrap());

        assert_eq!(link_name_for_port(port_id), "proteus49377");
    }

    fn sample_blueprint(generation: u64) -> PortBlueprint {
        PortBlueprint {
            port_id: PortId(Uuid::from_bytes([0x77; 16])),
            network_id: NetworkId::TRITON_VPC,
            schema_version: PORT_BLUEPRINT_SCHEMA_V0,
            generation: Generation::new(generation),
            limits: PortLimits::DEFAULT,
            link: ClientLinkConfig {
                mtu: 1500,
                mac_address: Some([0x02, 0x00, 0x00, 0xde, 0xad, 0x01]),
                vlan_id: None,
            },
            plugin_config: PluginConfigBytes::new(NetworkId::TRITON_VPC, 1, Vec::new()),
        }
    }

    #[test]
    fn ensure_started_creates_applies_starts_and_reports_status() {
        let proteus = ProteusClient::new(FakeTransport::new());
        let blueprint = sample_blueprint(7);

        let status = proteus.ensure_started(&blueprint, None).unwrap();

        assert_eq!(status.summary.port_id, blueprint.port_id);
        assert_eq!(status.summary.state, PortState::Running);
        assert_eq!(status.summary.applied_generation, Generation::new(7));
        assert_eq!(status.summary.apply_status, BlueprintApplyStatus::Applied);
        assert_eq!(status.generation.applied_generation, Generation::new(7));
        assert_eq!(
            status.generation.apply_status,
            BlueprintApplyStatus::Applied
        );
    }

    #[test]
    fn ensure_started_is_idempotent_for_existing_applied_generation() {
        let proteus = ProteusClient::new(FakeTransport::new());
        let blueprint = sample_blueprint(3);

        proteus.ensure_started(&blueprint, None).unwrap();
        let status = proteus.ensure_started(&blueprint, None).unwrap();

        assert_eq!(status.summary.state, PortState::Running);
        assert_eq!(status.generation.applied_generation, Generation::new(3));
    }

    #[test]
    fn pause_and_delete_cleanup_is_idempotent() {
        let proteus = ProteusClient::new(FakeTransport::new());
        let blueprint = sample_blueprint(5);

        proteus.ensure_started(&blueprint, None).unwrap();
        proteus.pause_port(blueprint.port_id).unwrap();
        let paused = proteus.dump_status(blueprint.port_id).unwrap();
        assert_eq!(paused.summary.state, PortState::Paused);

        proteus.cleanup_port(blueprint.port_id).unwrap();
        proteus.cleanup_port(blueprint.port_id).unwrap();
        assert!(proteus.dump_status(blueprint.port_id).is_err());
    }
}
