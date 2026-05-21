// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end loopback test for the migration state machines (LM-2).
//!
//! Connects an `OutboundMigration` and an `InboundMigration` via the
//! in-memory channel transport (no WebSocket / tokio-tungstenite
//! involvement), uses `MockVmm` instances backed by deterministic
//! patterns, and asserts every page made it across, the hashes
//! agree, and the captured device-state blobs round-trip.

use std::sync::Arc;
use std::sync::Mutex;

use tritond_vmm_migrate::transport::inmem;
use tritond_vmm_migrate::vmm_dev::mock::MockVmm;
use tritond_vmm_migrate::vmm_dev::{MemLayout, MemRegion};
use tritond_vmm_migrate::{
    InboundMigration, NoopSourceHooks, OutboundMigration, PAGE_SIZE, Phase, SharedVmm, SourceHooks,
    StateBlobs, TargetHooks,
};

/// Tiny VM shape: 1 vCPU, 64 KiB lowmem, 64 KiB highmem. Big enough
/// to exercise the multi-batch loop (`BATCH_SIZE = 64` pages =
/// 256 KiB per batch, so this is a single batch per region, but the
/// region-walk + alignment + decode paths all fire).
fn tiny_layout() -> MemLayout {
    MemLayout {
        num_cpus: 1,
        lowmem_size: 16 * PAGE_SIZE,
        highmem_size: 16 * PAGE_SIZE,
    }
}

/// Hooks that record the phase sequence so we can assert the
/// state machine walks the protocol in the expected order.
#[derive(Default, Clone)]
struct RecordingHooks {
    inner: Arc<Mutex<RecordingInner>>,
}

#[derive(Default)]
struct RecordingInner {
    phases: Vec<Phase>,
    bytes: u64,
    pages: u64,
}

impl RecordingHooks {
    fn snapshot(&self) -> (Vec<Phase>, u64, u64) {
        let g = self.inner.lock().unwrap();
        (g.phases.clone(), g.bytes, g.pages)
    }
}

impl SourceHooks for RecordingHooks {
    fn phase(&mut self, phase: Phase) {
        self.inner.lock().unwrap().phases.push(phase);
    }
    fn pause_complete_ts_ns(&mut self) -> u64 {
        // Deterministic so the test asserts on it later.
        1_700_000_000_000_000_000
    }
    fn switch_complete(&mut self, _ts: u64) {}
    fn pages_pushed(&mut self, pages: u64, bytes: u64) {
        let mut g = self.inner.lock().unwrap();
        g.pages += pages;
        g.bytes += bytes;
    }
}

impl TargetHooks for RecordingHooks {
    fn phase(&mut self, phase: Phase) {
        self.inner.lock().unwrap().phases.push(phase);
    }
    fn pause_complete(&mut self, _src_ts: u64) -> u64 {
        // Caller-recorded `target_activated_at`. In a real
        // tritonagent this would happen after Proteus `start_port`
        // + bhyve `resume_vm` returned.
        1_700_000_000_500_000_000
    }
    fn pages_received(&mut self, pages: u64, bytes: u64) {
        let mut g = self.inner.lock().unwrap();
        g.pages += pages;
        g.bytes += bytes;
    }
}

#[tokio::test]
async fn loopback_migrates_known_pattern_end_to_end() {
    let layout = tiny_layout();
    // Source has the deterministic pattern; target starts zeroed.
    // After the migration the target's memory must equal what the
    // source had.
    let src_vmm: SharedVmm =
        Arc::new(MockVmm::with_pattern(layout).with_time_blob(b"VMMTIMEDATA".to_vec()));
    let dst_vmm: SharedVmm = Arc::new(MockVmm::filled(layout, 0));

    let (src_t, dst_t) = inmem::channel_pair(64);

    let src_hooks = RecordingHooks::default();
    let dst_hooks = RecordingHooks::default();

    let blobs = StateBlobs {
        time_data: b"VMMTIMEDATA".to_vec(),
        kern_state: b"KERN_NVLIST".to_vec(),
        dev_state: b"DEV_NVLIST".to_vec(),
    };

    let src_recording = src_hooks.clone();
    let dst_recording = dst_hooks.clone();
    let src_vmm_for_assert = src_vmm.clone();
    let dst_vmm_for_assert = dst_vmm.clone();

    let src = tokio::spawn(async move {
        let out = OutboundMigration::new(src_t, src_vmm, blobs, src_hooks);
        out.run().await
    });
    let dst = tokio::spawn(async move {
        let inb = InboundMigration::new(dst_t, dst_vmm, dst_hooks);
        inb.run().await
    });

    src.await.expect("join src").expect("src run");
    let captured = dst.await.expect("join dst").expect("dst run");

    // Captured blobs round-tripped intact.
    assert_eq!(captured.time_data, b"VMMTIMEDATA");
    assert_eq!(captured.kern_state, b"KERN_NVLIST");
    assert_eq!(captured.dev_state, b"DEV_NVLIST");

    // Memory contents match end-to-end.
    let src_low = mock_snapshot(&src_vmm_for_assert, MemRegion::Lowmem);
    let dst_low = mock_snapshot(&dst_vmm_for_assert, MemRegion::Lowmem);
    assert_eq!(src_low, dst_low, "lowmem mismatch");
    let src_high = mock_snapshot(&src_vmm_for_assert, MemRegion::Highmem);
    let dst_high = mock_snapshot(&dst_vmm_for_assert, MemRegion::Highmem);
    assert_eq!(src_high, dst_high, "highmem mismatch");

    // Phase sequence on both sides walked the full protocol in
    // order. The Pause-Complete phase only fires from the source
    // side; the target observes via PauseSignal+PauseComplete
    // inside the Pause phase. So both see the same 8 phases.
    let (src_phases, _, src_pages) = src_recording.snapshot();
    let (dst_phases, _, dst_pages) = dst_recording.snapshot();
    let expected = vec![
        Phase::Sync,
        Phase::Pause,
        Phase::RamPush,
        Phase::RamHash,
        Phase::TimeData,
        Phase::DeviceState,
        Phase::Finish,
        Phase::Complete,
    ];
    assert_eq!(src_phases, expected, "source phase order");
    assert_eq!(dst_phases, expected, "target phase order");
    // Both sides report the same page-count progress.
    assert_eq!(src_pages, dst_pages);
    // 16 pages lowmem + 16 pages highmem = 32 pages total.
    assert_eq!(src_pages, 32);
}

#[tokio::test]
async fn loopback_rejects_cpu_mismatch() {
    let src_vmm: SharedVmm = Arc::new(MockVmm::with_pattern(MemLayout {
        num_cpus: 4,
        lowmem_size: 4 * PAGE_SIZE,
        highmem_size: 0,
    }));
    let dst_vmm: SharedVmm = Arc::new(MockVmm::filled(
        MemLayout {
            num_cpus: 2,
            lowmem_size: 4 * PAGE_SIZE,
            highmem_size: 0,
        },
        0,
    ));

    let (src_t, dst_t) = inmem::channel_pair(16);

    let src = tokio::spawn(async move {
        let out = OutboundMigration::new(src_t, src_vmm, StateBlobs::default(), NoopSourceHooks);
        out.run().await
    });
    let dst = tokio::spawn(async move {
        let inb = InboundMigration::new(dst_t, dst_vmm, RecordingHooks::default());
        inb.run().await
    });

    let dst_result = dst.await.expect("join dst");
    let src_result = src.await.expect("join src");
    assert!(dst_result.is_err(), "target must reject");
    assert!(src_result.is_err(), "source sees peer error or close");
}

fn mock_snapshot(vmm: &SharedVmm, region: MemRegion) -> Vec<u8> {
    // The shared trait object hides the concrete type; downcast
    // via Arc::as_any is intrusive, so instead we re-derive the
    // memory contents by reading every page through the trait.
    let layout = vmm.mem_layout();
    let len = match region {
        MemRegion::Lowmem => layout.lowmem_size,
        MemRegion::Highmem => layout.highmem_size,
    };
    let mut buf = vec![0u8; len];
    vmm.read_pages(region, 0, &mut buf).expect("snapshot read");
    buf
}
