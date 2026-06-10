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
//! agree, the captured device-state blobs round-trip, and the
//! import fence (`TargetHooks::state_received`) fires before the
//! target tells the source the cutover happened.

use std::io;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use tritond_vmm_migrate::transport::inmem;
use tritond_vmm_migrate::vmm_dev::mock::MockVmm;
use tritond_vmm_migrate::vmm_dev::{MemLayout, MemRegion};
use tritond_vmm_migrate::{
    InboundMigration, Message, MigrateError, NoopSourceHooks, OutboundMigration, PAGE_SIZE, Phase,
    SharedVmm, SourceHooks, StateBlobs, TargetCaptured, TargetHooks, Transport, VmmDev,
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

/// The `target_activated_at` timestamp the recording target hooks
/// return from `state_received`.
const TARGET_ACTIVATED_TS: u64 = 1_700_000_000_500_000_000;

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
    state_received_blobs: Option<TargetCaptured>,
    switch_complete_ts: Option<u64>,
}

impl RecordingHooks {
    fn snapshot(&self) -> (Vec<Phase>, u64, u64) {
        let g = self.inner.lock().unwrap();
        (g.phases.clone(), g.bytes, g.pages)
    }

    fn state_received_blobs(&self) -> Option<TargetCaptured> {
        self.inner.lock().unwrap().state_received_blobs.clone()
    }

    fn switch_complete_ts(&self) -> Option<u64> {
        self.inner.lock().unwrap().switch_complete_ts
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
    fn switch_complete(&mut self, ts: u64) {
        self.inner.lock().unwrap().switch_complete_ts = Some(ts);
    }
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
    fn pause_complete(&mut self, _src_ts: u64) {
        // Progress only; the import fence is `state_received`.
    }
    fn state_received(&mut self, blobs: &TargetCaptured) -> Result<u64, MigrateError> {
        // Caller-recorded `target_activated_at`. In a real
        // tritonagent this is where bhyve `import-state`, Proteus
        // `start_port`, and `resume-vm` happen.
        self.inner.lock().unwrap().state_received_blobs = Some(blobs.clone());
        Ok(TARGET_ACTIVATED_TS)
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

    // The import fence fired and got the same blobs run() returned.
    let fence_blobs = dst_recording
        .state_received_blobs()
        .expect("state_received fired");
    assert_eq!(fence_blobs.time_data, captured.time_data);
    assert_eq!(fence_blobs.kern_state, captured.kern_state);
    assert_eq!(fence_blobs.dev_state, captured.dev_state);

    // The source's switch_complete hook carries the timestamp the
    // target's state_received returned.
    assert_eq!(
        src_recording.switch_complete_ts(),
        Some(TARGET_ACTIVATED_TS)
    );

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

// ──────────────────────────────────────────────────────────────────
// Import-fence ordering (LM-2b). A transport wrapper logs every
// outbound target message into the same event log the fence hook
// writes to, making "state_received ran before SwitchComplete went
// on the wire" directly observable.
// ──────────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct EventLog(Arc<Mutex<Vec<String>>>);

impl EventLog {
    fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }
    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct LoggingTransport<T> {
    inner: T,
    log: EventLog,
}

#[async_trait]
impl<T: Transport> Transport for LoggingTransport<T> {
    async fn send(&mut self, msg: Message) -> io::Result<()> {
        self.log.push(format!("send:{}", tag_name(&msg)));
        self.inner.send(msg).await
    }
    async fn recv(&mut self) -> io::Result<Option<Message>> {
        self.inner.recv().await
    }
    async fn close(&mut self) -> io::Result<()> {
        self.inner.close().await
    }
}

fn tag_name(msg: &Message) -> &'static str {
    match msg {
        Message::Okay => "Okay",
        Message::Error(_) => "Error",
        Message::SwitchComplete(_) => "SwitchComplete",
        Message::MemDone => "MemDone",
        _ => "other",
    }
}

/// Target hooks that log the fence invocation into the shared
/// transport event log (and optionally fail the import).
struct FenceHooks {
    log: EventLog,
    fail_import: bool,
}

impl TargetHooks for FenceHooks {
    fn state_received(&mut self, _blobs: &TargetCaptured) -> Result<u64, MigrateError> {
        self.log.push("hook:state_received");
        if self.fail_import {
            Err(MigrateError::Transport(io::Error::other(
                "import refused by test hook",
            )))
        } else {
            Ok(42)
        }
    }
}

#[tokio::test]
async fn state_received_fires_before_switch_complete_is_sent() {
    let layout = tiny_layout();
    let src_vmm: SharedVmm = Arc::new(MockVmm::with_pattern(layout));
    let dst_vmm: SharedVmm = Arc::new(MockVmm::filled(layout, 0));
    let (src_t, dst_t) = inmem::channel_pair(64);

    let log = EventLog::default();
    let dst_t = LoggingTransport {
        inner: dst_t,
        log: log.clone(),
    };
    let dst_hooks = FenceHooks {
        log: log.clone(),
        fail_import: false,
    };

    let src_hooks = RecordingHooks::default();
    let src_recording = src_hooks.clone();

    let src = tokio::spawn(async move {
        OutboundMigration::new(src_t, src_vmm, StateBlobs::default(), src_hooks)
            .run()
            .await
    });
    let dst =
        tokio::spawn(async move { InboundMigration::new(dst_t, dst_vmm, dst_hooks).run().await });

    src.await.expect("join src").expect("src run");
    dst.await.expect("join dst").expect("dst run");

    let entries = log.entries();
    let fence = entries
        .iter()
        .position(|e| e == "hook:state_received")
        .expect("fence hook logged");
    let switch = entries
        .iter()
        .position(|e| e == "send:SwitchComplete")
        .expect("SwitchComplete sent");
    assert!(
        fence < switch,
        "state_received must run before SwitchComplete goes on the wire: {entries:?}"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| *e == "send:SwitchComplete")
            .count(),
        1,
        "exactly one SwitchComplete"
    );
    // The fence's returned timestamp is what the source observed.
    assert_eq!(src_recording.switch_complete_ts(), Some(42));
}

#[tokio::test]
async fn state_received_error_aborts_before_switch_complete() {
    let layout = tiny_layout();
    let src_vmm: SharedVmm = Arc::new(MockVmm::with_pattern(layout));
    let dst_vmm: SharedVmm = Arc::new(MockVmm::filled(layout, 0));
    let (src_t, dst_t) = inmem::channel_pair(64);

    let log = EventLog::default();
    let dst_t = LoggingTransport {
        inner: dst_t,
        log: log.clone(),
    };
    let dst_hooks = FenceHooks {
        log: log.clone(),
        fail_import: true,
    };

    let src = tokio::spawn(async move {
        OutboundMigration::new(src_t, src_vmm, StateBlobs::default(), NoopSourceHooks)
            .run()
            .await
    });
    let dst =
        tokio::spawn(async move { InboundMigration::new(dst_t, dst_vmm, dst_hooks).run().await });

    let dst_result = dst.await.expect("join dst");
    let src_result = src.await.expect("join src");

    assert!(
        matches!(dst_result, Err(MigrateError::Transport(_))),
        "target surfaces the hook's error: {dst_result:?}"
    );
    match src_result {
        Err(MigrateError::PeerError(msg)) => {
            assert!(
                msg.contains("state import failed"),
                "source sees the import failure: {msg}"
            );
        }
        other => panic!("source must see a peer error, got {other:?}"),
    }

    let entries = log.entries();
    assert!(
        entries.iter().any(|e| e == "hook:state_received"),
        "fence hook ran"
    );
    assert!(
        entries.iter().any(|e| e == "send:Error"),
        "target told the source"
    );
    assert!(
        !entries.iter().any(|e| e == "send:SwitchComplete"),
        "SwitchComplete must never follow a failed import: {entries:?}"
    );
}

/// Delegates to a real `MockVmm` but corrupts the RAM hash, forcing
/// the target's verification step to fail.
struct BadHashVmm(MockVmm);

impl VmmDev for BadHashVmm {
    fn mem_layout(&self) -> MemLayout {
        self.0.mem_layout()
    }
    fn read_pages(&self, region: MemRegion, offset: usize, dst: &mut [u8]) -> io::Result<()> {
        self.0.read_pages(region, offset, dst)
    }
    fn write_pages(&self, region: MemRegion, offset: usize, src: &[u8]) -> io::Result<()> {
        self.0.write_pages(region, offset, src)
    }
    fn hash_all_ram(&self) -> io::Result<u64> {
        Ok(self.0.hash_all_ram()? ^ 1)
    }
    fn export_time(&self) -> io::Result<Vec<u8>> {
        self.0.export_time()
    }
}

#[tokio::test]
async fn ram_hash_mismatch_is_fatal_and_skips_the_import_fence() {
    let layout = tiny_layout();
    let src_vmm: SharedVmm = Arc::new(MockVmm::with_pattern(layout));
    let dst_vmm: SharedVmm = Arc::new(BadHashVmm(MockVmm::filled(layout, 0)));
    let (src_t, dst_t) = inmem::channel_pair(64);

    let log = EventLog::default();
    let dst_t = LoggingTransport {
        inner: dst_t,
        log: log.clone(),
    };
    let dst_hooks = FenceHooks {
        log: log.clone(),
        fail_import: false,
    };

    let src = tokio::spawn(async move {
        OutboundMigration::new(src_t, src_vmm, StateBlobs::default(), NoopSourceHooks)
            .run()
            .await
    });
    let dst =
        tokio::spawn(async move { InboundMigration::new(dst_t, dst_vmm, dst_hooks).run().await });

    let dst_result = dst.await.expect("join dst");
    assert!(
        matches!(dst_result, Err(MigrateError::RamHashMismatch { .. })),
        "hash mismatch must be a hard error: {dst_result:?}"
    );
    let src_result = src.await.expect("join src");
    assert!(src_result.is_err(), "source must not complete");

    let entries = log.entries();
    assert!(
        !entries.iter().any(|e| e == "hook:state_received"),
        "import fence must not run after a hash mismatch: {entries:?}"
    );
    assert!(
        !entries.iter().any(|e| e == "send:SwitchComplete"),
        "SwitchComplete must never follow a hash mismatch: {entries:?}"
    );
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
