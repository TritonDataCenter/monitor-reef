// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Wire-protocol messages for bhyve live migration.
//!
//! Ported from the legacy `vmm-migrate-agent` codec (April 2026). Each
//! WebSocket binary frame is `[payload...][tag_byte]` — the tag byte
//! at the end identifies the message type.
//!
//! Two new variants for the saga switch-action fence
//! (`we-need-to-build-ancient-scone.md` §F.1):
//!
//! * `PauseComplete` — source agent signals it has fully paused +
//!   drained its viona rings, ready for target to activate.
//! * `SwitchComplete` — target agent signals the cutover is done,
//!   guest is resumed, source can release the dataset.
//!
//! These ride on the same WebSocket as the data path so the timing
//! fence around the cutover does not have to round-trip through
//! tritond + FDB (which would push downtime past 1s budget).

use serde::{Deserialize, Serialize};

/// Bit-flag on `PageBatch.flags` indicating the payload is
/// zstd-compressed.
pub const PAGE_BATCH_FLAG_ZSTD: u32 = 1;

/// Migration-protocol message.
///
/// The on-the-wire ordering matches the legacy agent's codec; new
/// agent-specific tags use `0x80+` to avoid collision with the
/// in-tree bhyve-userspace protocol numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Acknowledgement.
    Okay,
    /// Error with a free-form description.
    Error(String),
    /// JSON-serialised structured payload. Used for protocol
    /// negotiation (`{protocols: [...]}` / `{protocol: ...}`), the
    /// `MigrationPreamble`, the VMM_TIME blob, and the kernel +
    /// device-state nvlists.
    Serialized(Vec<u8>),
    /// Batch of guest RAM pages.
    PageBatch {
        /// Guest physical address of the first page in the batch.
        base_gpa: u64,
        /// Number of pages in `data`. `data.len() / PAGE_SIZE` after
        /// optional decompression.
        page_count: u32,
        /// Bitmask of [`PAGE_BATCH_FLAG_ZSTD`] etc. Future flags MUST
        /// stay in this u32 so the wire shape is stable.
        flags: u32,
        /// Raw page bytes, optionally zstd-compressed (see `flags`).
        data: Vec<u8>,
    },
    /// Sparse-page request: target lists GPAs the next batch should
    /// fetch from source. Pre-copy convergence used this; the
    /// pause-first protocol does not — kept on the wire for
    /// compatibility with future opt-in pre-copy.
    MemFetch(Vec<u64>),
    /// Source: "no more pages this pass".
    MemEnd,
    /// Target: "I've consumed the pass; you can send the next or
    /// the close handshake".
    MemDone,
    /// Source: "I have paused vCPUs + drained device rings; the
    /// next pass is final".
    PauseSignal,
    /// xxh3-64 hash of source guest RAM, computed while paused.
    /// Target verifies against its own RAM before resuming vCPUs.
    RamHash(u64),
    /// **New for LM-2.** Source agent has fully paused + drained
    /// AND its Proteus port is `pause`d (the on-wire fence the
    /// target waits on before `start_port`'ing its own NIC, per
    /// plan §F.1 switch sequence steps 1–3). Carries a monotonic
    /// timestamp (nanoseconds since unix epoch) the target's
    /// audit row records as `pause_complete_at`.
    PauseComplete(u64),
    /// **New for LM-2.** Target agent has activated its Proteus
    /// port + resumed the guest's vCPUs (plan §F.1 step 6). The
    /// source uses this to release the dataset / NICs / ZFS quota
    /// hold. Carries a monotonic timestamp recorded as
    /// `target_activated_at` for the split-brain detection rule
    /// (plan §H.4: `pause_complete_at < target_activated_at`).
    SwitchComplete(u64),
}

// Tag bytes. The first block (0x00–0x7f) is shared with the legacy
// agent + the in-tree bhyve userspace protocol so they wire-compat.
// 0x80+ is the agent-specific extension space.
const TAG_OKAY: u8 = 0;
const TAG_ERROR: u8 = 1;
const TAG_SERIALIZED: u8 = 2;
const TAG_MEM_FETCH: u8 = 7;
const TAG_MEM_END: u8 = 9;
const TAG_MEM_DONE: u8 = 10;
const TAG_PAGE_BATCH: u8 = 11;
const TAG_PAUSE_SIGNAL: u8 = 12;
const TAG_RAM_HASH: u8 = 0x80;
const TAG_PAUSE_COMPLETE: u8 = 0x81;
const TAG_SWITCH_COMPLETE: u8 = 0x82;

/// Error returned by [`Message::decode`] for malformed frames.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("empty frame")]
    Empty,
    #[error("unknown tag byte: 0x{0:02x}")]
    UnknownTag(u8),
    #[error("payload too short for tag 0x{tag:02x}: have {have} bytes, need {need}")]
    Short { tag: u8, have: usize, need: usize },
    #[error("PageBatch header malformed")]
    PageBatchHeader,
}

impl Message {
    /// Encode the message into a single WebSocket binary frame
    /// payload (the trailing byte is the tag).
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Message::Okay => vec![TAG_OKAY],
            Message::Error(msg) => {
                let mut buf = msg.as_bytes().to_vec();
                buf.push(TAG_ERROR);
                buf
            }
            Message::Serialized(data) => {
                let mut buf = data.clone();
                buf.push(TAG_SERIALIZED);
                buf
            }
            Message::PageBatch {
                base_gpa,
                page_count,
                flags,
                data,
            } => {
                let mut buf = Vec::with_capacity(16 + data.len() + 1);
                buf.extend_from_slice(&base_gpa.to_le_bytes());
                buf.extend_from_slice(&page_count.to_le_bytes());
                buf.extend_from_slice(&flags.to_le_bytes());
                buf.extend_from_slice(data);
                buf.push(TAG_PAGE_BATCH);
                buf
            }
            Message::MemFetch(gpas) => {
                let mut buf = Vec::with_capacity(gpas.len() * 8 + 1);
                for gpa in gpas {
                    buf.extend_from_slice(&gpa.to_le_bytes());
                }
                buf.push(TAG_MEM_FETCH);
                buf
            }
            Message::MemEnd => vec![TAG_MEM_END],
            Message::MemDone => vec![TAG_MEM_DONE],
            Message::PauseSignal => vec![TAG_PAUSE_SIGNAL],
            Message::RamHash(h) => {
                let mut buf = Vec::with_capacity(9);
                buf.extend_from_slice(&h.to_le_bytes());
                buf.push(TAG_RAM_HASH);
                buf
            }
            Message::PauseComplete(ts_ns) => {
                let mut buf = Vec::with_capacity(9);
                buf.extend_from_slice(&ts_ns.to_le_bytes());
                buf.push(TAG_PAUSE_COMPLETE);
                buf
            }
            Message::SwitchComplete(ts_ns) => {
                let mut buf = Vec::with_capacity(9);
                buf.extend_from_slice(&ts_ns.to_le_bytes());
                buf.push(TAG_SWITCH_COMPLETE);
                buf
            }
        }
    }

    /// Decode a single WebSocket binary-frame payload back into a
    /// [`Message`].
    pub fn decode(data: &[u8]) -> Result<Self, DecodeError> {
        if data.is_empty() {
            return Err(DecodeError::Empty);
        }
        let tag = data[data.len() - 1];
        let payload = &data[..data.len() - 1];

        match tag {
            TAG_OKAY => Ok(Message::Okay),
            TAG_ERROR => Ok(Message::Error(
                String::from_utf8_lossy(payload).into_owned(),
            )),
            TAG_SERIALIZED => Ok(Message::Serialized(payload.to_vec())),
            TAG_PAGE_BATCH => {
                if payload.len() < 16 {
                    return Err(DecodeError::PageBatchHeader);
                }
                let base_gpa = u64::from_le_bytes(payload[0..8].try_into().expect("8 bytes"));
                let page_count = u32::from_le_bytes(payload[8..12].try_into().expect("4 bytes"));
                let flags = u32::from_le_bytes(payload[12..16].try_into().expect("4 bytes"));
                let data = payload[16..].to_vec();
                Ok(Message::PageBatch {
                    base_gpa,
                    page_count,
                    flags,
                    data,
                })
            }
            TAG_MEM_FETCH => {
                let mut gpas = Vec::new();
                for chunk in payload.chunks_exact(8) {
                    gpas.push(u64::from_le_bytes(chunk.try_into().expect("8 bytes")));
                }
                Ok(Message::MemFetch(gpas))
            }
            TAG_MEM_END => Ok(Message::MemEnd),
            TAG_MEM_DONE => Ok(Message::MemDone),
            TAG_PAUSE_SIGNAL => Ok(Message::PauseSignal),
            TAG_RAM_HASH => {
                if payload.len() != 8 {
                    return Err(DecodeError::Short {
                        tag,
                        have: payload.len(),
                        need: 8,
                    });
                }
                Ok(Message::RamHash(u64::from_le_bytes(
                    payload.try_into().expect("8 bytes"),
                )))
            }
            TAG_PAUSE_COMPLETE => {
                if payload.len() != 8 {
                    return Err(DecodeError::Short {
                        tag,
                        have: payload.len(),
                        need: 8,
                    });
                }
                Ok(Message::PauseComplete(u64::from_le_bytes(
                    payload.try_into().expect("8 bytes"),
                )))
            }
            TAG_SWITCH_COMPLETE => {
                if payload.len() != 8 {
                    return Err(DecodeError::Short {
                        tag,
                        have: payload.len(),
                        need: 8,
                    });
                }
                Ok(Message::SwitchComplete(u64::from_le_bytes(
                    payload.try_into().expect("8 bytes"),
                )))
            }
            other => Err(DecodeError::UnknownTag(other)),
        }
    }
}

/// Preamble exchanged during the Sync phase. Source -> Target.
///
/// `num_cpus` must match exactly on both sides; bhyve migration
/// doesn't support CPU-count change across hosts.
/// `mem_size` is informational on the wire (the target rederives
/// from its own bhyve status); kept for cross-checking and to
/// surface mismatches as a clean error rather than a wedged ioctl.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MigrationPreamble {
    pub num_cpus: u32,
    pub mem_size: u64,
}

/// Serialised VMM_TIME blob. bhyve packs an nvlist; we treat the
/// payload as opaque on this side and round-trip it byte-for-byte.
///
/// Kept as a typed struct in legacy code; in vnext the
/// `Message::Serialized(blob)` variant carries the opaque bytes
/// directly, so this struct is exposed only for callers who want a
/// strongly-named place to attach docs (e.g. the saga audit log).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimeData(pub Vec<u8>);

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: Message) {
        let bytes = msg.encode();
        let decoded = Message::decode(&bytes).expect("decode");
        assert_eq!(msg, decoded, "roundtrip mismatch");
    }

    #[test]
    fn roundtrip_okay() {
        roundtrip(Message::Okay);
    }

    #[test]
    fn roundtrip_error() {
        roundtrip(Message::Error("upstream eof".into()));
    }

    #[test]
    fn roundtrip_serialized() {
        roundtrip(Message::Serialized(vec![0u8, 1, 2, 254, 255]));
    }

    #[test]
    fn roundtrip_page_batch_uncompressed() {
        roundtrip(Message::PageBatch {
            base_gpa: 0x1000,
            page_count: 4,
            flags: 0,
            data: vec![0xab; 4 * 4096],
        });
    }

    #[test]
    fn roundtrip_page_batch_compressed_flag() {
        // The compression itself happens in the state-machine
        // layer; the codec preserves whatever flag bits are set.
        roundtrip(Message::PageBatch {
            base_gpa: 0x4_0000_0000, // highmem base
            page_count: 1,
            flags: PAGE_BATCH_FLAG_ZSTD,
            data: vec![0u8; 1024],
        });
    }

    #[test]
    fn roundtrip_mem_fetch_empty() {
        roundtrip(Message::MemFetch(Vec::new()));
    }

    #[test]
    fn roundtrip_mem_fetch_some() {
        roundtrip(Message::MemFetch(vec![0x1000, 0x2000, 0xdead_beef_0000]));
    }

    #[test]
    fn roundtrip_mem_end_and_done() {
        roundtrip(Message::MemEnd);
        roundtrip(Message::MemDone);
    }

    #[test]
    fn roundtrip_pause_signal() {
        roundtrip(Message::PauseSignal);
    }

    #[test]
    fn roundtrip_ram_hash() {
        roundtrip(Message::RamHash(0xdead_beef_cafe_f00d));
        roundtrip(Message::RamHash(0));
        roundtrip(Message::RamHash(u64::MAX));
    }

    #[test]
    fn roundtrip_pause_complete_lm2() {
        roundtrip(Message::PauseComplete(1_700_000_000_000_000_000));
    }

    #[test]
    fn roundtrip_switch_complete_lm2() {
        roundtrip(Message::SwitchComplete(1_700_000_000_500_000_000));
    }

    #[test]
    fn decode_empty_is_error() {
        assert!(matches!(Message::decode(&[]), Err(DecodeError::Empty)));
    }

    #[test]
    fn decode_unknown_tag_is_error() {
        // 0xfe is in the agent-extension range but not assigned to
        // any variant. Must surface as UnknownTag, not panic.
        let payload = vec![0u8, 0u8, 0xfe];
        match Message::decode(&payload) {
            Err(DecodeError::UnknownTag(0xfe)) => {}
            other => panic!("expected UnknownTag(0xfe), got {other:?}"),
        }
    }

    #[test]
    fn decode_short_ram_hash_is_error() {
        // RamHash needs exactly 8 bytes of payload before the tag.
        let payload = vec![0u8, 0u8, TAG_RAM_HASH];
        match Message::decode(&payload) {
            Err(DecodeError::Short { tag, have, need }) => {
                assert_eq!(tag, TAG_RAM_HASH);
                assert_eq!(have, 2);
                assert_eq!(need, 8);
            }
            other => panic!("expected Short, got {other:?}"),
        }
    }

    #[test]
    fn decode_short_page_batch_header_is_error() {
        // Less than 16 bytes (8 base_gpa + 4 page_count + 4 flags)
        // before the tag.
        let payload = vec![0u8; 10 + 1]; // 10 bytes payload + tag
        let mut p = payload;
        let last = p.len() - 1;
        p[last] = TAG_PAGE_BATCH;
        assert!(matches!(
            Message::decode(&p),
            Err(DecodeError::PageBatchHeader)
        ));
    }

    #[test]
    fn preamble_serde_roundtrip() {
        let p = MigrationPreamble {
            num_cpus: 4,
            mem_size: 4 * 1024 * 1024 * 1024,
        };
        let json = serde_json::to_vec(&p).unwrap();
        let back: MigrationPreamble = serde_json::from_slice(&json).unwrap();
        assert_eq!(p, back);
    }
}
