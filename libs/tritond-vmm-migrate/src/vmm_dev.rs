// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Abstraction over the kernel `/dev/vmm/<name>` device + bhyve's
//! `data_read(VMM_TIME)` ioctl.
//!
//! The legacy `vmm-migrate-agent::vmm_dev` module wrapped the illumos
//! ioctls directly and exposed raw `*mut u8` mmap pointers to the
//! source/destination orchestrators. That works for the agent's
//! original "one big main()" shape but is hostile to testing: every
//! state-machine test path needs an actual SmartOS host with a
//! running bhyve VM.
//!
//! Here we lift it as a trait. The state machines (`OutboundMigration`,
//! `InboundMigration`) bind to this trait, the SmartOS impl
//! ([`SmartOsVmm`], target-os-gated) calls the real ioctls, and the
//! in-memory [`mock::MockVmm`] backs guest RAM with a `Vec<u8>` per
//! region for unit + loopback tests.
//!
//! The trait is intentionally narrow: only the operations the
//! state machines actually call. Adding methods means adding a mock
//! shim alongside the real ioctl, so we resist the temptation to
//! mirror the entire ioctl surface here.

use std::io;
use std::sync::{Arc, Mutex};

use crate::protocol::PAGE_SIZE;

/// Memory region the state machines read from / write to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemRegion {
    /// `[0, lowmem_size)`.
    Lowmem,
    /// `[HIGHMEM_BASE_GPA, HIGHMEM_BASE_GPA + highmem_size)`.
    Highmem,
}

/// Shape of guest memory as reported by the bhyve control socket's
/// `status` command. Lowmem + highmem are the two contiguous
/// regions; the BIOS hole between them is not addressable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemLayout {
    pub num_cpus: u32,
    pub lowmem_size: usize,
    pub highmem_size: usize,
}

impl MemLayout {
    /// Total guest RAM in bytes.
    pub fn total_bytes(&self) -> usize {
        self.lowmem_size + self.highmem_size
    }

    /// How many pages of size [`PAGE_SIZE`] this region holds.
    pub fn pages_in(&self, region: MemRegion) -> usize {
        match region {
            MemRegion::Lowmem => self.lowmem_size / PAGE_SIZE,
            MemRegion::Highmem => self.highmem_size / PAGE_SIZE,
        }
    }
}

/// The narrow surface the migration state machines need from the
/// kernel /dev/vmm device.
///
/// Implementations are expected to be `Send + Sync`; the trait is
/// not `async` because the operations are either pure compute
/// (mock) or thin ioctl wrappers (SmartOS). The state machines that
/// drive this run in their own tokio task and we hand off to a
/// `spawn_blocking` only at the call site if needed — keeping the
/// trait sync means the mock doesn't have to fake an async runtime.
pub trait VmmDev: Send + Sync {
    /// Memory layout the source/target negotiate over and the page
    /// loop walks through. Returned shape must remain stable for
    /// the lifetime of one migration.
    fn mem_layout(&self) -> MemLayout;

    /// Copy `dst.len()` bytes starting at `offset` inside `region`.
    /// `dst.len()` must be a multiple of [`PAGE_SIZE`] and the range
    /// must fit inside the region.
    ///
    /// Source agent uses this to populate `Message::PageBatch.data`
    /// before optional zstd compression.
    fn read_pages(&self, region: MemRegion, offset: usize, dst: &mut [u8]) -> io::Result<()>;

    /// Copy `src.len()` bytes starting at `offset` inside `region`.
    /// Same alignment + range invariants as [`Self::read_pages`].
    ///
    /// Target agent uses this to materialise pages received over the
    /// wire (after zstd decompression).
    fn write_pages(&self, region: MemRegion, offset: usize, src: &[u8]) -> io::Result<()>;

    /// Combined xxh3-64 of every byte of lowmem then every byte of
    /// highmem. **Caller must ensure the guest is paused before
    /// calling**; the SmartOS impl will read mmap'd memory directly
    /// and an in-flight vCPU write would produce a torn read.
    ///
    /// The source computes this after pause; the target computes
    /// it after RAM receive but before bhyve's `import-state`
    /// kicks vCPUs. Mismatch is a hard error.
    fn hash_all_ram(&self) -> io::Result<u64>;

    /// Export the kernel-side VMM_TIME nvlist. Source-only.
    ///
    /// Returned bytes are an opaque illumos packed nvlist; the
    /// target hands them straight to bhyve's `import-state` which
    /// reads its own destination time live and applies the
    /// cross-host TSC + wall-clock adjustment.
    fn export_time(&self) -> io::Result<Vec<u8>>;
}

/// Shared trait object the state machines hold internally.
pub type SharedVmm = Arc<dyn VmmDev>;

/// In-memory `VmmDev` for unit + loopback tests. Backs each
/// memory region with a `Vec<u8>` so the source state machine can
/// read + hash and the target state machine can write + re-hash.
///
/// Wrap the inner state in a `Mutex` so the trait's `&self` reads
/// + writes can mutate safely; in tests there is only one thread
/// per side so contention is irrelevant.
pub mod mock {
    use super::*;

    /// In-memory mock backing for [`VmmDev`]. Construct via
    /// [`MockVmm::with_pattern`] when you want deterministic
    /// guest contents the source/target can hash-compare.
    pub struct MockVmm {
        layout: MemLayout,
        inner: Mutex<Inner>,
        time_blob: Vec<u8>,
    }

    struct Inner {
        lowmem: Vec<u8>,
        highmem: Vec<u8>,
    }

    impl MockVmm {
        /// Build a mock with the given layout and fill every byte
        /// of guest RAM with `byte`. Useful for deterministic
        /// loopback tests where source pre-fills with one pattern
        /// and target starts zeroed.
        pub fn filled(layout: MemLayout, byte: u8) -> Self {
            let lowmem = vec![byte; layout.lowmem_size];
            let highmem = vec![byte; layout.highmem_size];
            Self {
                layout,
                inner: Mutex::new(Inner { lowmem, highmem }),
                time_blob: Vec::new(),
            }
        }

        /// Build a mock and fill each region with a deterministic
        /// pattern (`offset & 0xff`) so source/target hashes are
        /// easy to verify in tests.
        pub fn with_pattern(layout: MemLayout) -> Self {
            let lowmem: Vec<u8> = (0..layout.lowmem_size).map(|i| (i & 0xff) as u8).collect();
            let highmem: Vec<u8> = (0..layout.highmem_size).map(|i| (i & 0xff) as u8).collect();
            Self {
                layout,
                inner: Mutex::new(Inner { lowmem, highmem }),
                time_blob: Vec::new(),
            }
        }

        /// Pre-seed the bytes the source's `export_time` returns.
        pub fn with_time_blob(mut self, blob: Vec<u8>) -> Self {
            self.time_blob = blob;
            self
        }

        /// Snapshot the current contents of one region. Tests use
        /// this on the target side after a loopback to assert the
        /// source's pattern arrived intact.
        pub fn region_snapshot(&self, region: MemRegion) -> Vec<u8> {
            let g = self.inner.lock().expect("mutex poisoned");
            match region {
                MemRegion::Lowmem => g.lowmem.clone(),
                MemRegion::Highmem => g.highmem.clone(),
            }
        }
    }

    impl VmmDev for MockVmm {
        fn mem_layout(&self) -> MemLayout {
            self.layout
        }

        fn read_pages(&self, region: MemRegion, offset: usize, dst: &mut [u8]) -> io::Result<()> {
            check_aligned(offset, dst.len())?;
            let g = self.inner.lock().expect("mutex poisoned");
            let src = match region {
                MemRegion::Lowmem => &g.lowmem,
                MemRegion::Highmem => &g.highmem,
            };
            let end = offset
                .checked_add(dst.len())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range overflow"))?;
            if end > src.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("out of range: end={end} region_len={}", src.len()),
                ));
            }
            dst.copy_from_slice(&src[offset..end]);
            Ok(())
        }

        fn write_pages(&self, region: MemRegion, offset: usize, src: &[u8]) -> io::Result<()> {
            check_aligned(offset, src.len())?;
            let mut g = self.inner.lock().expect("mutex poisoned");
            let dst = match region {
                MemRegion::Lowmem => &mut g.lowmem,
                MemRegion::Highmem => &mut g.highmem,
            };
            let end = offset
                .checked_add(src.len())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range overflow"))?;
            if end > dst.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("out of range: end={end} region_len={}", dst.len()),
                ));
            }
            dst[offset..end].copy_from_slice(src);
            Ok(())
        }

        fn hash_all_ram(&self) -> io::Result<u64> {
            let g = self.inner.lock().expect("mutex poisoned");
            Ok(crate::protocol::hash_guest_ram(
                Some(&g.lowmem),
                Some(&g.highmem),
            ))
        }

        fn export_time(&self) -> io::Result<Vec<u8>> {
            Ok(self.time_blob.clone())
        }
    }

    fn check_aligned(offset: usize, len: usize) -> io::Result<()> {
        if offset % PAGE_SIZE != 0 || len % PAGE_SIZE != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("page-misaligned access: offset={offset} len={len}"),
            ));
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────
// SmartOS real impl. Gated on target_os = "illumos" so dev builds on
// macOS / Linux still compile (with only the trait + mock exposed).
//
// LM-2 ships the trait + mock + the surrounding state machine. The
// ioctl-level FFI is a near-verbatim port of the legacy
// vmm-migrate-agent::vmm_dev module; we leave it as a TODO sub-task
// for LM-2 follow-up so the rest of the crate's wire structure can
// be exercised + reviewed before the unsafe FFI lands.
// ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "illumos")]
mod smartos {
    // The full ioctl wrapper from `vmm-migrate-agent::vmm_dev` lifts
    // here. The wrapper structs (`VmmDev` handle, mmap pointers,
    // per-vCPU register set) all carry illumos-specific definitions
    // (`VMM_IOC_BASE`, `VM_DATA_READ`, `VDC_VMM_TIME`, etc.) that
    // don't belong on macOS/Linux build machines.
    //
    // LM-2 ships the trait + mock so the OutboundMigration /
    // InboundMigration state machines, the loopback test, and the
    // codec are review-ready. The SmartOS impl lands as a follow-up
    // ("LM-2b") so the unsafe FFI block can be reviewed on its own
    // and run against the dev hosts (.10 / .40 / .41 per the
    // tritond_smartos_access memory note).
    //
    // Until then this module is intentionally empty; downstream
    // callers reach for the mock or feature-gate on
    // `cfg(target_os = "illumos")` themselves.
}
