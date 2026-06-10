// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Protocol-level constants and pure helpers.
//!
//! Ported from the legacy `vmm-migrate-agent::protocol` module with
//! the pre-copy convergence loop deleted. Per the LM plan §F-6 we
//! ship pause-first only — the convergence iterators and threshold
//! constants are not carried over (they were the load-bearing
//! producers of the guest panics the legacy team eventually rolled
//! back).

use xxhash_rust::xxh3::Xxh3;

/// 4 KiB. Page size every part of the protocol works in.
pub const PAGE_SIZE: usize = 4096;

/// Pages per `Message::PageBatch`. 64 pages = 256 KiB raw payload;
/// after zstd-1 compression typical batches land at 30-80 KiB —
/// well under the tungstenite default 16 MiB frame cap and small
/// enough that a dropped frame retransmits cheaply.
pub const BATCH_SIZE: usize = 64;

/// zstd compression level applied to each `PageBatch`. Level 1 is
/// the lowest non-zero level; it picks up the easy ratio wins
/// (mostly-zero guest pages collapse 100x) without spending CPU we'd
/// rather give back to the migration's wall-clock.
pub const ZSTD_LEVEL: i32 = 1;

/// Wire protocol version string. Source advertises this in the
/// `{protocols: [...]}` offer; target picks one and replies with
/// `{protocol: ...}`.
///
/// The `cn-bhyve-compatible` placement filter (LM-0,
/// `tritond-placement::filter::CnBhyveCompatible`) compares this
/// against each candidate target's reported
/// `CapacityView::vmm_protocol_version` so version-incompatible
/// CNs never become target candidates.
pub const PROTOCOL_V0: &str = "vmm-migrate-ron/0";

/// Guest-physical address where highmem starts. Bhyve places highmem
/// at 4 GiB so the BIOS hole between 0xC0000000 and 0x100000000 is
/// preserved.
pub const HIGHMEM_BASE_GPA: u64 = 4 * 1024 * 1024 * 1024;

/// LM-4 — size of one `Message::ZfsChunk` payload, in bytes.
///
/// 256 KiB strikes a balance:
///
/// * Each WebSocket binary frame has a small (~2-14 byte) framing
///   overhead — at 256 KiB chunks that's <0.005% overhead.
/// * The frame is well under tungstenite's default 16 MiB cap and
///   well under the OS socket buffer, so backpressure on the
///   sender propagates cleanly to `zfs send` via stdout-pipe
///   blocking.
/// * A dropped connection mid-chunk requires retransmitting at
///   most this much — the dataset transfer has no resume, so the
///   chunk granularity is the unit of work we're willing to
///   lose on a transient.
pub const ZFS_CHUNK_SIZE: usize = 256 * 1024;

/// Hash one contiguous byte slice with xxh3-64.
///
/// `data` must be the entire region the caller wants represented;
/// the chunk boundary inside the hasher is an implementation
/// detail. The function is `safe` because the input is a regular
/// slice — the dangerous `*const u8` form that the legacy code used
/// for raw mmap pointers is intentionally not exposed here; callers
/// must materialise the region into a slice (the `VmmDev` trait
/// implementations do this).
pub fn hash_region(data: &[u8]) -> u64 {
    let mut h = Xxh3::new();
    // The legacy code chunked in 4 KiB stack buffers to bound stack
    // usage when reading from a volatile mmap pointer. With a real
    // slice that doesn't apply, but we keep the chunked feed so the
    // hash result matches the legacy agent's byte-by-byte feed —
    // important because the same bytes hashed in different
    // increments still digest the same.
    for chunk in data.chunks(PAGE_SIZE) {
        h.update(chunk);
    }
    h.digest()
}

/// Hash two optional regions (typically lowmem + highmem) into a
/// single digest, in lowmem-then-highmem order. Both `None` digests
/// to the empty-input xxh3 value — a defined, stable seed-only
/// constant rather than zero; we don't special-case it because a
/// run with zero bytes is a configuration error the surrounding
/// state machine catches, not a wire-protocol case.
///
/// The source's full-RAM hash and the target's full-RAM hash must
/// produce the same digest if and only if every page made it across
/// the wire intact. The target compares these *before* starting
/// vCPUs; mismatch is a fatal error that aborts the migration.
pub fn hash_guest_ram(lowmem: Option<&[u8]>, highmem: Option<&[u8]>) -> u64 {
    let mut h = Xxh3::new();
    for region in [lowmem, highmem].into_iter().flatten() {
        for chunk in region.chunks(PAGE_SIZE) {
            h.update(chunk);
        }
    }
    h.digest()
}

/// Streaming counterpart of [`hash_guest_ram`] for callers that
/// cannot materialise a whole memory region as one slice: the
/// SmartOS `VmmDev` reads guest RAM through a bounded copy buffer
/// because a 64 GiB guest must not require a 64 GiB heap allocation
/// to hash. xxh3's streaming digest is independent of update-chunk
/// boundaries, so feeding the same bytes in any chunking produces
/// the same digest as the slice helpers above;
/// `streaming_hash_matches_slice_helpers` pins that contract.
pub struct RamHasher(Xxh3);

impl RamHasher {
    pub fn new() -> Self {
        Self(Xxh3::new())
    }

    /// Feed the next bytes, in lowmem-then-highmem region order.
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn digest(&self) -> u64 {
        self.0.digest()
    }
}

impl Default for RamHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_region_matches_one_shot() {
        let bytes: Vec<u8> = (0..4096u32).map(|i| (i & 0xff) as u8).collect();
        let chunked = hash_region(&bytes);
        // Direct Xxh3 over the full slice — must match the chunked
        // path bit-for-bit (this is the contract the source/target
        // verification step relies on).
        let mut h = Xxh3::new();
        h.update(&bytes);
        let one_shot = h.digest();
        assert_eq!(chunked, one_shot);
    }

    #[test]
    fn hash_guest_ram_stable_for_empty_input() {
        // Both None must produce a deterministic digest (the
        // empty-input xxh3) so two implementations stay in lockstep.
        // We don't pin the literal value — Xxh3 reserves the right
        // to change its seed across versions — but we do pin
        // "deterministic across calls in this process".
        assert_eq!(hash_guest_ram(None, None), hash_guest_ram(None, None));
    }

    #[test]
    fn hash_guest_ram_concatenates_regions() {
        let low = vec![0xAAu8; PAGE_SIZE];
        let high = vec![0x55u8; PAGE_SIZE];
        let combined = hash_guest_ram(Some(&low), Some(&high));
        // Same as hashing the concatenation.
        let mut joined = low.clone();
        joined.extend_from_slice(&high);
        assert_eq!(combined, hash_region(&joined));
    }

    #[test]
    fn streaming_hash_matches_slice_helpers() {
        let low: Vec<u8> = (0..3 * PAGE_SIZE).map(|i| (i & 0xff) as u8).collect();
        let high: Vec<u8> = (0..2 * PAGE_SIZE)
            .map(|i| ((i >> 3) & 0xff) as u8)
            .collect();
        let expected = hash_guest_ram(Some(&low), Some(&high));

        // Deliberately odd, page-misaligned chunk sizes; the
        // digest must not depend on how the bytes were fed.
        for chunk in [1usize, 7, 1000, PAGE_SIZE, PAGE_SIZE + 13] {
            let mut h = RamHasher::new();
            for region in [&low, &high] {
                for piece in region.chunks(chunk) {
                    h.update(piece);
                }
            }
            assert_eq!(h.digest(), expected, "chunk size {chunk}");
        }
    }
}
