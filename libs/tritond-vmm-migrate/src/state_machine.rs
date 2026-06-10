// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `OutboundMigration` (source) and `InboundMigration` (target)
//! state machines.
//!
//! Ported from the legacy `vmm-migrate-agent::{source,destination}`
//! modules to tokio-async and generic over the [`Transport`] trait.
//! Both sides walk the pause-first 7-phase protocol per the LM plan
//! §4 / `we-need-to-build-ancient-scone.md`:
//!
//! ```text
//!   Sync      ─ negotiate protocol, exchange preamble
//!   Pause     ─ source pauses viona + vCPUs + drains device I/O
//!   RamPush   ─ source pushes ALL pages while paused (single pass)
//!   RamHash   ─ source xxh3 of guest RAM; target verifies its copy
//!   TimeData  ─ source exports VMM_TIME; target hands to bhyve
//!   DevState  ─ source exports kernel+device nvlists; target imports
//!   Finish    ─ source: MemEnd; target: Okay; source: Okay; close
//! ```
//!
//! The LM-2 new `PauseComplete` / `SwitchComplete` codec messages
//! (§F.1 fence) are surfaced via callbacks so the tritonagent
//! migrate module (LM-3) can drive Proteus port pause/start in the
//! tight cutover window without round-tripping the saga. The
//! target's cutover side effects (bhyve `import-state`, Proteus
//! `start_port`, `resume-vm`) run inside
//! [`TargetHooks::state_received`], which fires after the
//! device-state blobs land and before the Finish handshake, so the
//! source is never told the switch happened (`SwitchComplete`)
//! until the target can actually run the guest.
//!
//! Bhyve control-socket interactions (`pause_vm`, `drain_devices`,
//! `export_state`, `import_state`, `resume_vm`) are not wired here;
//! the state machine emits typed `Step` events the caller drives.
//! That way the in-memory loopback test doesn't need a fake
//! [`BhyveCtl`] and the tritonagent module can interleave proteus
//! / ZFS work between phases without touching this file.

use std::io;
use std::sync::Arc;

use crate::codec::{Message, MigrationPreamble, PAGE_BATCH_FLAG_ZSTD};
use crate::protocol::{BATCH_SIZE, HIGHMEM_BASE_GPA, PAGE_SIZE, PROTOCOL_V0, ZSTD_LEVEL};
use crate::transport::Transport;
use crate::vmm_dev::{MemLayout, MemRegion, SharedVmm};

/// All failure modes the state machines can surface. Maps roughly
/// 1:1 to the wire phase that produced them so the tritonagent
/// caller's audit log entry can name the failing step without
/// string-matching.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("transport: {0}")]
    Transport(#[from] io::Error),
    #[error("peer closed transport before {phase}")]
    PeerClosed { phase: &'static str },
    #[error("decode: {0}")]
    Decode(#[from] crate::codec::DecodeError),
    #[error("peer error: {0}")]
    PeerError(String),
    #[error("unexpected message in {phase}: {got}")]
    Unexpected {
        phase: &'static str,
        got: &'static str,
    },
    #[error("protocol mismatch: source={src} target={dst}")]
    ProtocolMismatch { src: String, dst: String },
    #[error("cpu-count mismatch: source={src} target={dst}")]
    CpuMismatch { src: u32, dst: u32 },
    #[error("ram-hash mismatch: source=0x{src:016x} target=0x{dst:016x}")]
    RamHashMismatch { src: u64, dst: u64 },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zstd: {0}")]
    Zstd(String),
    /// A caller-side hook failed (e.g. the target's
    /// [`TargetHooks::state_received`] import fence). Distinct from
    /// [`Self::Transport`] so the audit row can tell "the wire
    /// died" apart from "bhyve refused the import".
    #[error("cutover hook: {0}")]
    Hook(String),
}

/// Wire-side phase the state machine is currently in. The
/// tritonagent caller surfaces this on the
/// `migration/progress/{id}/{seq}` event log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Sync,
    Pause,
    RamPush,
    RamHash,
    TimeData,
    DeviceState,
    Finish,
    Complete,
}

/// Callback the source state machine invokes at each phase
/// boundary. Lets the tritonagent caller drive bhyve_ctl /
/// Proteus side effects + push progress events without the state
/// machine needing to know about them.
///
/// `pause_complete_at_ns` is the monotonic timestamp the caller
/// should drive at the post-pause / post-drain instant; it is sent
/// back to the target as `Message::PauseComplete` so the target's
/// audit row records the same instant.
pub trait SourceHooks: Send {
    /// Called when entering a new phase.
    fn phase(&mut self, _phase: Phase) {}

    /// Called after the pause + drain has happened on the source.
    /// Must return the timestamp (ns since unix epoch) the caller
    /// recorded for `pause_complete_at`. This goes onto the wire
    /// as `Message::PauseComplete` so the target's audit row gets
    /// the same instant.
    fn pause_complete_ts_ns(&mut self) -> u64 {
        0
    }

    /// Called after the target's `SwitchComplete` arrives. The
    /// caller uses this to release source-side dataset / NICs /
    /// ZFS quota.
    fn switch_complete(&mut self, _target_activated_at_ns: u64) {}

    /// Progress callback fired after every batch.
    fn pages_pushed(&mut self, _pages: u64, _bytes: u64) {}
}

/// Default hooks impl — useful when the caller doesn't care.
pub struct NoopSourceHooks;
impl SourceHooks for NoopSourceHooks {}

/// Target-side counterpart of [`SourceHooks`].
pub trait TargetHooks: Send {
    fn phase(&mut self, _phase: Phase) {}

    /// Called when the source's `PauseComplete` arrives. Progress
    /// only: guest RAM has not been received yet, so no import /
    /// resume / dataplane work may happen here; that belongs in
    /// [`Self::state_received`].
    fn pause_complete(&mut self, _source_paused_at_ns: u64) {}

    /// The import fence. Called after the device-state blobs are
    /// captured and verified RAM is in place, but BEFORE the Finish
    /// handshake. The caller drives bhyve `import-state`, Proteus
    /// `start_port`, and `resume-vm` here; the returned timestamp
    /// (ns since unix epoch) is what the caller recorded as
    /// `target_activated_at` and goes on the wire as
    /// `SwitchComplete`. Returning an error aborts the migration
    /// before the source is ever told the cutover happened.
    fn state_received(&mut self, _blobs: &TargetCaptured) -> Result<u64, MigrateError> {
        Ok(0)
    }

    fn pages_received(&mut self, _pages: u64, _bytes: u64) {}
}

pub struct NoopTargetHooks;
impl TargetHooks for NoopTargetHooks {}

/// Optional time-data + device-state payloads carried alongside the
/// state-machine drive. On a real CN the tritonagent supplies these
/// from the bhyve control socket; in the loopback test they're
/// pre-baked.
#[derive(Debug, Clone, Default)]
pub struct StateBlobs {
    pub time_data: Vec<u8>,
    pub kern_state: Vec<u8>,
    pub dev_state: Vec<u8>,
}

/// Outbound (source-side) migration state machine.
///
/// Drives the source half of the 7-phase protocol over `transport`.
/// `vmm` provides the guest memory + time export; `blobs.kern_state`
/// + `blobs.dev_state` are the pre-captured bhyve state nvlists
/// (the caller is expected to have called `bhyve_ctl.export_state`
/// already; we don't take a `BhyveCtl` here to keep this layer pure
/// + testable).
pub struct OutboundMigration<T: Transport, H: SourceHooks> {
    transport: T,
    vmm: SharedVmm,
    blobs: StateBlobs,
    hooks: H,
}

impl<T: Transport, H: SourceHooks> OutboundMigration<T, H> {
    pub fn new(transport: T, vmm: SharedVmm, blobs: StateBlobs, hooks: H) -> Self {
        Self {
            transport,
            vmm,
            blobs,
            hooks,
        }
    }

    /// Run the source half to completion.
    pub async fn run(mut self) -> Result<(), MigrateError> {
        let layout = self.vmm.mem_layout();

        // ── Phase 1: Sync ──
        self.hooks.phase(Phase::Sync);
        send(
            &mut self.transport,
            serialised(&serde_json::json!({
                "protocols": [PROTOCOL_V0],
            }))?,
        )
        .await?;

        let sel = expect_serialised(&mut self.transport, "sync").await?;
        let sel: serde_json::Value = serde_json::from_slice(&sel)?;
        let selected = sel
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if selected != PROTOCOL_V0 {
            return Err(MigrateError::ProtocolMismatch {
                src: PROTOCOL_V0.to_string(),
                dst: selected,
            });
        }

        let preamble = MigrationPreamble {
            num_cpus: layout.num_cpus,
            mem_size: layout.total_bytes() as u64,
        };
        send(
            &mut self.transport,
            Message::Serialized(serde_json::to_vec(&preamble)?),
        )
        .await?;
        expect_okay(&mut self.transport, "sync").await?;

        // ── Phase 2: Pause ──
        //
        // The actual `bhyve_ctl::pause_devices/pause_vm/drain_devices`
        // calls happen on the caller side; by the time the caller
        // invokes us they've already returned. We signal the target
        // and surface the timestamp the caller recorded.
        self.hooks.phase(Phase::Pause);
        let pause_ts = self.hooks.pause_complete_ts_ns();
        send(&mut self.transport, Message::PauseSignal).await?;
        send(&mut self.transport, Message::PauseComplete(pause_ts)).await?;

        // ── Phase 3: RamPush (single full pass while paused) ──
        self.hooks.phase(Phase::RamPush);
        if layout.lowmem_size > 0 {
            self.push_region(MemRegion::Lowmem, 0, layout.lowmem_size)
                .await?;
        }
        if layout.highmem_size > 0 {
            self.push_region(MemRegion::Highmem, HIGHMEM_BASE_GPA, layout.highmem_size)
                .await?;
        }
        send(&mut self.transport, Message::MemEnd).await?;
        expect_mem_done(&mut self.transport, "ram-push").await?;

        // ── Phase 4: RamHash ──
        self.hooks.phase(Phase::RamHash);
        let src_hash = self.vmm.hash_all_ram()?;
        send(&mut self.transport, Message::RamHash(src_hash)).await?;

        // ── Phase 5: TimeData ──
        self.hooks.phase(Phase::TimeData);
        let time_data = if !self.blobs.time_data.is_empty() {
            self.blobs.time_data.clone()
        } else {
            self.vmm.export_time()?
        };
        send(&mut self.transport, Message::Serialized(time_data)).await?;

        // ── Phase 6: DeviceState ──
        self.hooks.phase(Phase::DeviceState);
        send(
            &mut self.transport,
            Message::Serialized(self.blobs.kern_state.clone()),
        )
        .await?;
        send(
            &mut self.transport,
            Message::Serialized(self.blobs.dev_state.clone()),
        )
        .await?;

        // ── Phase 7: Finish ──
        self.hooks.phase(Phase::Finish);
        send(&mut self.transport, Message::MemEnd).await?;
        expect_okay(&mut self.transport, "finish").await?;
        send(&mut self.transport, Message::Okay).await?;

        // Wait for the target's `SwitchComplete` (the cutover
        // fence). Surface its timestamp via the hook so the
        // caller's audit row records `target_activated_at`.
        let switch_ts = recv(&mut self.transport, "switch-complete").await?;
        match switch_ts {
            Message::SwitchComplete(ts) => self.hooks.switch_complete(ts),
            other => {
                return Err(MigrateError::Unexpected {
                    phase: "switch-complete",
                    got: variant_name(&other),
                });
            }
        }

        self.hooks.phase(Phase::Complete);
        let _ = self.transport.close().await;
        Ok(())
    }

    async fn push_region(
        &mut self,
        region: MemRegion,
        base_gpa: u64,
        len: usize,
    ) -> Result<(), MigrateError> {
        let num_pages = len / PAGE_SIZE;
        let mut offset_pages = 0usize;
        while offset_pages < num_pages {
            let count = BATCH_SIZE.min(num_pages - offset_pages);
            let raw_bytes = count * PAGE_SIZE;
            let offset_bytes = offset_pages * PAGE_SIZE;

            let mut buf = vec![0u8; raw_bytes];
            self.vmm.read_pages(region, offset_bytes, &mut buf)?;

            let (data, flags) = compress_batch(&buf);
            let gpa = base_gpa + offset_bytes as u64;
            send(
                &mut self.transport,
                Message::PageBatch {
                    base_gpa: gpa,
                    page_count: count as u32,
                    flags,
                    data,
                },
            )
            .await?;
            self.hooks.pages_pushed(count as u64, raw_bytes as u64);
            offset_pages += count;
        }
        Ok(())
    }
}

/// Inbound (target-side) migration state machine.
pub struct InboundMigration<T: Transport, H: TargetHooks> {
    transport: T,
    vmm: SharedVmm,
    blobs: TargetCaptured,
    hooks: H,
}

/// What the inbound machine captures from the source. The caller
/// (tritonagent migrate module) feeds the kern+dev nvlists into
/// `bhyve_ctl::import_state` inside [`TargetHooks::state_received`],
/// which fires before the Finish handshake; by the time `run`
/// returns, the import has already happened.
#[derive(Debug, Default, Clone)]
pub struct TargetCaptured {
    pub time_data: Vec<u8>,
    pub kern_state: Vec<u8>,
    pub dev_state: Vec<u8>,
}

impl<T: Transport, H: TargetHooks> InboundMigration<T, H> {
    pub fn new(transport: T, vmm: SharedVmm, hooks: H) -> Self {
        Self {
            transport,
            vmm,
            blobs: TargetCaptured::default(),
            hooks,
        }
    }

    /// Run the target half to completion. The captured
    /// `(time_data, kern_state, dev_state)` blobs are handed to
    /// [`TargetHooks::state_received`] before the Finish handshake
    /// (where the caller drives `bhyve_ctl::import_state`) and also
    /// returned for inspection.
    pub async fn run(mut self) -> Result<TargetCaptured, MigrateError> {
        let layout = self.vmm.mem_layout();

        // ── Phase 1: Sync ──
        self.hooks.phase(Phase::Sync);
        let offer = expect_serialised(&mut self.transport, "sync").await?;
        let offer: serde_json::Value = serde_json::from_slice(&offer)?;
        let protocols: Vec<&str> = offer
            .get("protocols")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        if !protocols.iter().any(|p| *p == PROTOCOL_V0) {
            send(
                &mut self.transport,
                Message::Error("protocol mismatch".into()),
            )
            .await?;
            return Err(MigrateError::ProtocolMismatch {
                src: protocols.join(","),
                dst: PROTOCOL_V0.to_string(),
            });
        }
        send(
            &mut self.transport,
            Message::Serialized(serde_json::to_vec(&serde_json::json!({
                "protocol": PROTOCOL_V0,
            }))?),
        )
        .await?;

        let preamble_bytes = expect_serialised(&mut self.transport, "sync").await?;
        let preamble: MigrationPreamble = serde_json::from_slice(&preamble_bytes)?;
        if preamble.num_cpus != layout.num_cpus {
            let msg = format!(
                "cpu mismatch: src={} dst={}",
                preamble.num_cpus, layout.num_cpus,
            );
            send(&mut self.transport, Message::Error(msg.clone())).await?;
            return Err(MigrateError::CpuMismatch {
                src: preamble.num_cpus,
                dst: layout.num_cpus,
            });
        }
        send(&mut self.transport, Message::Okay).await?;

        // ── Phase 2: Pause wait ──
        self.hooks.phase(Phase::Pause);
        match recv(&mut self.transport, "pause").await? {
            Message::PauseSignal => {}
            other => {
                return Err(MigrateError::Unexpected {
                    phase: "pause",
                    got: variant_name(&other),
                });
            }
        }
        // LM-2 new: expect the post-drain timestamp directly after
        // PauseSignal. This is the fence the target's caller uses
        // to gate Proteus `start_port`.
        let source_pause_ts = match recv(&mut self.transport, "pause-complete").await? {
            Message::PauseComplete(ts) => ts,
            other => {
                return Err(MigrateError::Unexpected {
                    phase: "pause-complete",
                    got: variant_name(&other),
                });
            }
        };
        // Progress only: RAM hasn't arrived yet, so the caller
        // must not import or resume anything here. The cutover
        // side effects live in `state_received` below.
        self.hooks.pause_complete(source_pause_ts);

        // ── Phase 3: RamPush (receive single full pass) ──
        self.hooks.phase(Phase::RamPush);
        self.receive_ram_pass(&layout).await?;

        // ── Phase 4: RamHash ──
        self.hooks.phase(Phase::RamHash);
        let src_hash = match recv(&mut self.transport, "ram-hash").await? {
            Message::RamHash(h) => h,
            other => {
                return Err(MigrateError::Unexpected {
                    phase: "ram-hash",
                    got: variant_name(&other),
                });
            }
        };
        let dst_hash = self.vmm.hash_all_ram()?;
        if src_hash != dst_hash {
            return Err(MigrateError::RamHashMismatch {
                src: src_hash,
                dst: dst_hash,
            });
        }

        // ── Phase 5: TimeData ──
        self.hooks.phase(Phase::TimeData);
        self.blobs.time_data = expect_serialised(&mut self.transport, "time-data").await?;

        // ── Phase 6: DeviceState ──
        self.hooks.phase(Phase::DeviceState);
        self.blobs.kern_state = expect_serialised(&mut self.transport, "kern-state").await?;
        self.blobs.dev_state = expect_serialised(&mut self.transport, "dev-state").await?;

        // Import fence (LM-2b): the caller imports device state and
        // brings the target dataplane up NOW, before the Finish
        // handshake, so the source is only told the cutover is done
        // once the target can actually run the guest. Wire order is
        // unchanged: the source has already sent its post-state
        // MemEnd and is parked in its okay wait; we simply haven't
        // read it yet.
        let target_activated_ts = match self.hooks.state_received(&self.blobs) {
            Ok(ts) => ts,
            Err(e) => {
                send(
                    &mut self.transport,
                    Message::Error(format!("state import failed: {e}")),
                )
                .await?;
                return Err(e);
            }
        };

        // ── Phase 7: Finish ──
        self.hooks.phase(Phase::Finish);
        match recv(&mut self.transport, "finish").await? {
            Message::MemEnd => {}
            other => {
                return Err(MigrateError::Unexpected {
                    phase: "finish",
                    got: variant_name(&other),
                });
            }
        }
        send(&mut self.transport, Message::Okay).await?;
        expect_okay(&mut self.transport, "finish").await?;
        // Fence the source: tell it the cutover is done (so its
        // caller can release source-side resources). The target is
        // already live; `state_received` returned above.
        send(
            &mut self.transport,
            Message::SwitchComplete(target_activated_ts),
        )
        .await?;

        self.hooks.phase(Phase::Complete);
        let _ = self.transport.close().await;
        Ok(self.blobs)
    }

    async fn receive_ram_pass(&mut self, layout: &MemLayout) -> Result<(), MigrateError> {
        loop {
            let msg = recv(&mut self.transport, "ram-push").await?;
            match msg {
                Message::PageBatch {
                    base_gpa,
                    page_count,
                    flags,
                    data,
                } => {
                    self.handle_batch(layout, base_gpa, page_count, flags, &data)?;
                    self.hooks
                        .pages_received(page_count as u64, page_count as u64 * PAGE_SIZE as u64);
                }
                Message::MemEnd => {
                    send(&mut self.transport, Message::MemDone).await?;
                    return Ok(());
                }
                other => {
                    return Err(MigrateError::Unexpected {
                        phase: "ram-push",
                        got: variant_name(&other),
                    });
                }
            }
        }
    }

    fn handle_batch(
        &self,
        layout: &MemLayout,
        base_gpa: u64,
        page_count: u32,
        flags: u32,
        data: &[u8],
    ) -> Result<(), MigrateError> {
        let expected = page_count as usize * PAGE_SIZE;
        let raw: Vec<u8> = if flags & PAGE_BATCH_FLAG_ZSTD != 0 {
            zstd::bulk::decompress(data, expected)
                .map_err(|e| MigrateError::Zstd(format!("decompress: {e}")))?
        } else {
            data.to_vec()
        };
        if raw.len() != expected {
            return Err(MigrateError::Zstd(format!(
                "size mismatch: got {} expected {}",
                raw.len(),
                expected,
            )));
        }
        let (region, offset) = if base_gpa < HIGHMEM_BASE_GPA {
            if (base_gpa as usize + expected) > layout.lowmem_size {
                return Err(MigrateError::Unexpected {
                    phase: "ram-push",
                    got: "page out of lowmem range",
                });
            }
            (MemRegion::Lowmem, base_gpa as usize)
        } else {
            let offset = (base_gpa - HIGHMEM_BASE_GPA) as usize;
            if offset + expected > layout.highmem_size {
                return Err(MigrateError::Unexpected {
                    phase: "ram-push",
                    got: "page out of highmem range",
                });
            }
            (MemRegion::Highmem, offset)
        };
        self.vmm.write_pages(region, offset, &raw)?;
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────
// Wire helpers.
// ──────────────────────────────────────────────────────────────────

async fn send<T: Transport>(t: &mut T, msg: Message) -> Result<(), MigrateError> {
    t.send(msg).await.map_err(MigrateError::Transport)
}

async fn recv<T: Transport>(t: &mut T, phase: &'static str) -> Result<Message, MigrateError> {
    match t.recv().await? {
        Some(Message::Error(e)) => Err(MigrateError::PeerError(e)),
        Some(msg) => Ok(msg),
        None => Err(MigrateError::PeerClosed { phase }),
    }
}

async fn expect_okay<T: Transport>(t: &mut T, phase: &'static str) -> Result<(), MigrateError> {
    match recv(t, phase).await? {
        Message::Okay => Ok(()),
        other => Err(MigrateError::Unexpected {
            phase,
            got: variant_name(&other),
        }),
    }
}

async fn expect_serialised<T: Transport>(
    t: &mut T,
    phase: &'static str,
) -> Result<Vec<u8>, MigrateError> {
    match recv(t, phase).await? {
        Message::Serialized(b) => Ok(b),
        other => Err(MigrateError::Unexpected {
            phase,
            got: variant_name(&other),
        }),
    }
}

async fn expect_mem_done<T: Transport>(t: &mut T, phase: &'static str) -> Result<(), MigrateError> {
    match recv(t, phase).await? {
        Message::MemDone => Ok(()),
        other => Err(MigrateError::Unexpected {
            phase,
            got: variant_name(&other),
        }),
    }
}

fn serialised(v: &serde_json::Value) -> Result<Message, MigrateError> {
    Ok(Message::Serialized(serde_json::to_vec(v)?))
}

fn variant_name(msg: &Message) -> &'static str {
    match msg {
        Message::Okay => "Okay",
        Message::Error(_) => "Error",
        Message::Serialized(_) => "Serialized",
        Message::PageBatch { .. } => "PageBatch",
        Message::MemFetch(_) => "MemFetch",
        Message::MemEnd => "MemEnd",
        Message::MemDone => "MemDone",
        Message::PauseSignal => "PauseSignal",
        Message::RamHash(_) => "RamHash",
        Message::PauseComplete(_) => "PauseComplete",
        Message::SwitchComplete(_) => "SwitchComplete",
        // Memory-channel state machine never expects these on its
        // wire — the ZFS variants ride a separate Transport
        // instance (the `GET /migrate/{id}/zfs` listener). If one
        // shows up here it means the caller mis-wired the two
        // channels; surface the variant name so the audit log
        // says "ZfsChunk in ram-push phase".
        Message::ZfsChunk(_) => "ZfsChunk",
        Message::ZfsEnd => "ZfsEnd",
    }
}

fn compress_batch(raw: &[u8]) -> (Vec<u8>, u32) {
    match zstd::bulk::compress(raw, ZSTD_LEVEL) {
        Ok(c) if c.len() < raw.len() => (c, PAGE_BATCH_FLAG_ZSTD),
        _ => (raw.to_vec(), 0),
    }
}

// Avoid the unused-import warning when the file is being read by
// rustdoc / clippy on a platform that didn't pull `Arc` in via a
// public type. The state-machine's user-facing API does expose Arc
// via `SharedVmm`, so this is genuinely live.
#[doc(hidden)]
#[allow(dead_code)]
fn _arc_is_used(_: Arc<()>) {}
