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

/// Alignment guard shared by every implementation: the wire protocol
/// and the GPA → region math both assume whole-page transfers, so a
/// misaligned access is a caller bug, not a runtime condition.
fn check_aligned(offset: usize, len: usize) -> io::Result<()> {
    if offset % PAGE_SIZE != 0 || len % PAGE_SIZE != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("page-misaligned access: offset={offset} len={len}"),
        ));
    }
    Ok(())
}

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
}

// ──────────────────────────────────────────────────────────────────
// SmartOS real impl. Gated on target_os = "illumos" so dev builds on
// macOS / Linux still compile (with only the trait + mock exposed).
// ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "illumos")]
pub use smartos::SmartOsVmm;

#[cfg(target_os = "illumos")]
pub mod smartos {
    //! ioctl-backed [`VmmDev`] for a live bhyve guest, lifted from
    //! the legacy `vmm-migrate-agent::vmm_dev` module (hardware
    //! tested). Only the active migration surface is ported: map
    //! guest RAM, read `VDC_VMM_TIME`. The donor's per-vCPU
    //! register/segment/FPU/run-state ioctls and NPT dirty tracking
    //! are deliberately NOT lifted: they are dead code on the
    //! pause-first path (bhyve's `export-state` carries all vCPU +
    //! device state) and the donor's GET/SET register ioctl numbers
    //! are swapped relative to `vmm_dev.h`, so porting them would
    //! ship a landmine. The donor's `VM_PAUSE`/`VM_RESUME` are also
    //! skipped: pausing from the GZ deadlocks against bhyve's own
    //! `VM_DATA_WRITE` (see `BhyveCtl::pause_vm`); pause/resume go
    //! through the control socket.
    //!
    //! ioctl numbers and struct layouts mirror
    //! `usr/src/uts/intel/sys/vmm_dev.h` / `vmm_data.h` in
    //! illumos-joyent. The raw `nix::libc::ioctl`/`mmap` calls are
    //! used instead of nix's typed wrappers because the vmm ioctl
    //! numbers don't follow the `_IOWR` request-code encoding nix's
    //! macros generate.

    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::ptr;

    use nix::libc;

    use super::{MemLayout, MemRegion, VmmDev, check_aligned};
    use crate::protocol::{HIGHMEM_BASE_GPA, PAGE_SIZE, RamHasher};

    // From vmm_dev.h: VMM_IOC_BASE is (('v' << 16) | ('m' << 8)).
    const VMM_IOC_BASE: i32 = ((b'v' as i32) << 16) | ((b'm' as i32) << 8);
    const VM_DATA_READ: i32 = VMM_IOC_BASE | 0x22;
    const VM_DEVMEM_GETOFFSET: i32 = VMM_IOC_BASE | 0xff;

    // VM_DATA_XFER copy-direction flags + per-call size cap.
    const VDX_FLAG_WRITE_COPYOUT: u32 = 1 << 1;
    const VM_DATA_XFER_LIMIT: usize = 8192;

    /// `VDC_VMM_TIME` class id from vmm_data.h; version 1 is
    /// `vdi_time_info_v1` (48 bytes, comfortably under
    /// [`VM_DATA_XFER_LIMIT`], so the single fixed buffer below
    /// never needs the ENOSPC retry dance).
    const VDC_VMM_TIME: u16 = 13;
    const VDC_VMM_TIME_VERSION: u16 = 1;

    /// Well-known segid of the system memory segment.
    const VM_SYSMEM: i32 = 0;

    /// Mirrors `struct vm_data_xfer` (vmm_dev.h) exactly.
    #[repr(C)]
    struct VmDataXfer {
        vdx_vcpuid: i32,
        vdx_class: u16,
        vdx_version: u16,
        vdx_flags: u32,
        vdx_len: u32,
        vdx_result_len: u32,
        vdx_data: *mut c_void,
    }

    /// Mirrors `struct vm_devmem_offset` (vmm_dev.h) exactly.
    #[repr(C)]
    struct VmDevmemOffset {
        segid: i32,
        offset: i64,
    }

    /// One mmap'd guest memory region. Unmapped on drop so a failed
    /// migration doesn't leak multi-GiB mappings in a long-lived
    /// tritonagent process.
    struct Mapping {
        ptr: *mut u8,
        len: usize,
    }

    // SAFETY: the pointer refers to MAP_SHARED guest memory that the
    // kernel keeps valid for the lifetime of the mapping (the open
    // /dev/vmm fd lives alongside it in SmartOsVmm). All access goes
    // through volatile byte copies and the migration contract
    // serialises use (the source reads only after the guest is
    // paused, the target writes only before vCPUs start), so moving
    // or sharing the handle across threads is sound.
    unsafe impl Send for Mapping {}
    unsafe impl Sync for Mapping {}

    impl Mapping {
        fn map(fd: RawFd, offset: i64, len: usize) -> io::Result<Self> {
            // SAFETY: fd is an open /dev/vmm device and offset/len
            // come from the kernel's devmem offset + the guest's
            // negotiated memory layout; mmap validates the range.
            let ptr = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    offset as libc::off_t,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                ptr: ptr as *mut u8,
                len,
            })
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: ptr/len are exactly what mmap returned.
            unsafe {
                libc::munmap(self.ptr as *mut c_void, self.len);
            }
        }
    }

    /// [`VmmDev`] over a live `/dev/vmm/<name>` device.
    ///
    /// The caller resolves the vmm name (`SYSbhyve-<zoneid>`) and
    /// obtains the memory layout from the bhyve control socket's
    /// `status` command before constructing this; the kernel device
    /// itself doesn't tell us where lowmem ends.
    pub struct SmartOsVmm {
        fd: File,
        name: String,
        layout: MemLayout,
        lowmem: Option<Mapping>,
        highmem: Option<Mapping>,
    }

    impl SmartOsVmm {
        /// Open `/dev/vmm/<vmm_name>` and map both guest RAM
        /// regions. Lowmem maps at the sysmem devmem offset; highmem
        /// at that offset + 4 GiB, because the devmem offset space
        /// mirrors the GPA hole below 4 GiB.
        pub fn open(vmm_name: &str, layout: MemLayout) -> io::Result<Self> {
            let path = format!("/dev/vmm/{vmm_name}");
            let fd = OpenOptions::new().read(true).write(true).open(&path)?;

            let sysmem_offset = devmem_getoffset(fd.as_raw_fd(), VM_SYSMEM)?;
            let lowmem = if layout.lowmem_size > 0 {
                Some(Mapping::map(
                    fd.as_raw_fd(),
                    sysmem_offset,
                    layout.lowmem_size,
                )?)
            } else {
                None
            };
            let highmem = if layout.highmem_size > 0 {
                Some(Mapping::map(
                    fd.as_raw_fd(),
                    sysmem_offset + HIGHMEM_BASE_GPA as i64,
                    layout.highmem_size,
                )?)
            } else {
                None
            };

            Ok(Self {
                fd,
                name: vmm_name.to_string(),
                layout,
                lowmem,
                highmem,
            })
        }

        pub fn name(&self) -> &str {
            &self.name
        }

        fn region(&self, region: MemRegion) -> (Option<&Mapping>, usize) {
            match region {
                MemRegion::Lowmem => (self.lowmem.as_ref(), self.layout.lowmem_size),
                MemRegion::Highmem => (self.highmem.as_ref(), self.layout.highmem_size),
            }
        }

        /// Range-check an access and return the mapping. Alignment
        /// is checked separately by the trait methods; the hash
        /// path streams in arbitrary chunk sizes.
        fn mapping_for(
            &self,
            region: MemRegion,
            offset: usize,
            len: usize,
        ) -> io::Result<&Mapping> {
            let (mapping, region_len) = self.region(region);
            let end = offset
                .checked_add(len)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range overflow"))?;
            if end > region_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("out of range: end={end} region_len={region_len}"),
                ));
            }
            mapping.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "region not mapped"))
        }

        fn data_read(&self, vcpuid: i32, class: u16, version: u16) -> io::Result<Vec<u8>> {
            let mut buf = vec![0u8; VM_DATA_XFER_LIMIT];
            let mut xfer = VmDataXfer {
                vdx_vcpuid: vcpuid,
                vdx_class: class,
                vdx_version: version,
                vdx_flags: VDX_FLAG_WRITE_COPYOUT,
                vdx_len: buf.len() as u32,
                vdx_result_len: 0,
                vdx_data: buf.as_mut_ptr() as *mut c_void,
            };
            // SAFETY: xfer points at a live buffer sized vdx_len; the
            // kernel copies out at most that many bytes.
            let ret = unsafe { libc::ioctl(self.fd.as_raw_fd(), VM_DATA_READ as _, &mut xfer) };
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
            buf.truncate(xfer.vdx_result_len as usize);
            Ok(buf)
        }
    }

    impl VmmDev for SmartOsVmm {
        fn mem_layout(&self) -> MemLayout {
            self.layout
        }

        fn read_pages(&self, region: MemRegion, offset: usize, dst: &mut [u8]) -> io::Result<()> {
            check_aligned(offset, dst.len())?;
            if dst.is_empty() {
                return Ok(());
            }
            let mapping = self.mapping_for(region, offset, dst.len())?;
            // Byte-volatile copy, donor semantics: the compiler must
            // not coalesce or elide reads of guest memory.
            for (i, b) in dst.iter_mut().enumerate() {
                // SAFETY: mapping_for bounds-checked offset+len
                // against the mapped region.
                *b = unsafe { ptr::read_volatile(mapping.ptr.add(offset + i)) };
            }
            Ok(())
        }

        fn write_pages(&self, region: MemRegion, offset: usize, src: &[u8]) -> io::Result<()> {
            check_aligned(offset, src.len())?;
            if src.is_empty() {
                return Ok(());
            }
            let mapping = self.mapping_for(region, offset, src.len())?;
            for (i, &b) in src.iter().enumerate() {
                // SAFETY: mapping_for bounds-checked offset+len
                // against the mapped region.
                unsafe { ptr::write_volatile(mapping.ptr.add(offset + i), b) };
            }
            Ok(())
        }

        fn hash_all_ram(&self) -> io::Result<u64> {
            // Stream through a bounded buffer instead of
            // materialising whole regions: guest RAM can be tens
            // of GiB. 64 pages matches the wire batch size; the
            // chunking does not affect the digest (see RamHasher).
            const HASH_CHUNK: usize = 64 * PAGE_SIZE;
            let mut hasher = RamHasher::new();
            let mut buf = vec![0u8; HASH_CHUNK];
            for region in [MemRegion::Lowmem, MemRegion::Highmem] {
                let (mapping, region_len) = self.region(region);
                let Some(mapping) = mapping else {
                    continue;
                };
                let mut offset = 0usize;
                while offset < region_len {
                    let n = HASH_CHUNK.min(region_len - offset);
                    for (i, b) in buf[..n].iter_mut().enumerate() {
                        // SAFETY: offset + n <= region_len, the
                        // mapped length of this region.
                        *b = unsafe { ptr::read_volatile(mapping.ptr.add(offset + i)) };
                    }
                    hasher.update(&buf[..n]);
                    offset += n;
                }
            }
            Ok(hasher.digest())
        }

        fn export_time(&self) -> io::Result<Vec<u8>> {
            // vcpuid -1 selects the system-wide (non-per-vCPU)
            // device class, which VMM_TIME is.
            self.data_read(-1, VDC_VMM_TIME, VDC_VMM_TIME_VERSION)
        }
    }

    fn devmem_getoffset(fd: RawFd, segid: i32) -> io::Result<i64> {
        let mut dmo = VmDevmemOffset { segid, offset: 0 };
        // SAFETY: dmo is a live, correctly-laid-out vm_devmem_offset.
        let ret = unsafe { libc::ioctl(fd, VM_DEVMEM_GETOFFSET as _, &mut dmo) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(dmo.offset)
    }
}
