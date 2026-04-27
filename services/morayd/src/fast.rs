// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Fast-protocol (node-fast) codec.
//!
//! Wire layout per `node-fast/lib/fast_protocol.js`:
//!
//! ```text
//!     byte  0     1     2     3..6        7..10       11..14      15..
//!         VER  TYPE  STAT  MSGID(u32)  CRC(u32)   DLEN(u32)   DATA(JSON)
//! ```
//!
//! Fixed values we emit: `VER=2`, `TYPE=1 (JSON)`. CRC is the CRC-16 XMODEM
//! of the JSON payload in the bottom 16 bits of the 32-bit field (upper 16
//! zeroed). We accept any incoming CRC (clients run at several versions)
//! rather than rejecting on mismatch — node-fast's own server operates this
//! way in practice when `FAST_CHECKSUM_V1_V2` is in effect.

use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::codec::{Decoder, Encoder};

pub const FP_HEADER_SZ: usize = 15;
// Emit v1 on the wire. The old fast@2.8.2 bundled in legacy Triton services
// (moray client inside vmapi, cnapi, cloudapi, etc.) hard-codes v1 as the
// only accepted inbound version. Modern node-fast accepts both v1 and v2,
// so v1 is universally compatible. Our decoder still tolerates either.
pub const FP_VERSION_CURRENT: u8 = 1;
pub const FP_TYPE_JSON: u8 = 1;

pub const FP_STATUS_DATA: u8 = 1;
pub const FP_STATUS_END: u8 = 2;
pub const FP_STATUS_ERROR: u8 = 3;

/// Status field of a fast message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastStatus {
    Data,
    End,
    Error,
}

impl FastStatus {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            FP_STATUS_DATA => Some(Self::Data),
            FP_STATUS_END => Some(Self::End),
            FP_STATUS_ERROR => Some(Self::Error),
            _ => None,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Data => FP_STATUS_DATA,
            Self::End => FP_STATUS_END,
            Self::Error => FP_STATUS_ERROR,
        }
    }
}

/// Envelope placed inside the JSON payload of every fast message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastMeta {
    #[serde(default)]
    pub uts: u64,
    pub name: String,
}

/// The deserialized JSON data section of a fast message:
/// `{ "m": {"uts": ms, "name": "<rpc>"}, "d": <value> }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastData {
    pub m: FastMeta,
    pub d: Value,
}

/// One complete fast message: header + typed data section.
#[derive(Debug, Clone)]
pub struct FastMessage {
    pub id: u32,
    pub status: FastStatus,
    pub data: FastData,
}

impl FastMessage {
    /// Emit a DATA frame. node-fast requires `d` to be an array for DATA and
    /// END frames; each element is one streamed "row" of the response. We
    /// accept any JSON here and always wrap into a single-element array so
    /// callers don't have to remember.
    pub fn data(id: u32, rpc: &str, payload: Value) -> Self {
        let d = if payload.is_array() {
            payload
        } else {
            Value::Array(vec![payload])
        };
        Self {
            id,
            status: FastStatus::Data,
            data: FastData {
                m: FastMeta { uts: uts_now(), name: rpc.to_string() },
                d,
            },
        }
    }

    pub fn end(id: u32, rpc: &str) -> Self {
        Self {
            id,
            status: FastStatus::End,
            data: FastData {
                m: FastMeta { uts: uts_now(), name: rpc.to_string() },
                // `d` must be an array per the fast protocol spec; an empty
                // array is the canonical shape for End.
                d: Value::Array(Vec::new()),
            },
        }
    }

    pub fn error(id: u32, rpc: &str, err: Value) -> Self {
        Self {
            id,
            status: FastStatus::Error,
            data: FastData {
                m: FastMeta { uts: uts_now(), name: rpc.to_string() },
                d: err,
            },
        }
    }
}

fn uts_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Rewrite UTF-8 JSON so every non-ASCII code point becomes a
/// `\uXXXX` escape sequence. Input is assumed well-formed UTF-8
/// (serde_json output always is). Valid JSON inside and outside
/// strings — outside-string chars in well-formed JSON are already
/// ASCII.
fn ascii_escape_json(bytes: &[u8]) -> Vec<u8> {
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return bytes.to_vec(),
    };
    let mut out = Vec::with_capacity(bytes.len());
    for c in s.chars() {
        let cu = c as u32;
        if cu < 0x80 {
            out.push(cu as u8);
        } else if cu <= 0xFFFF {
            // JSON spec: \uXXXX for a BMP code point inside a string.
            out.extend_from_slice(format!("\\u{:04x}", cu).as_bytes());
        } else {
            // Above the BMP: surrogate pair per the JSON spec.
            let scalar = cu - 0x10000;
            let hi = 0xD800 + (scalar >> 10);
            let lo = 0xDC00 + (scalar & 0x3FF);
            out.extend_from_slice(
                format!("\\u{:04x}\\u{:04x}", hi, lo).as_bytes(),
            );
        }
    }
    out
}

fn crc16_ccitt_xmodem(bytes: &[u8]) -> u16 {
    // node-fast v2 uses CRC-16/XMODEM (poly 0x1021, init 0x0000,
    // no reflection, no xorout).
    crc16::State::<crc16::XMODEM>::calculate(bytes)
}

/// Errors returned by the fast codec. Non-fatal variants cause the current
/// frame to be skipped; fatal variants drop the connection.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("unsupported version {0}")]
    UnsupportedVersion(u8),
    #[error("unsupported type {0}")]
    UnsupportedType(u8),
    #[error("unknown status {0}")]
    UnknownStatus(u8),
    #[error("data-length {0} exceeds max {max}", max = MAX_DATA_LEN)]
    Overflow(u32),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Cap per-message payload. Node-fast has no hard cap but in practice a
/// Moray call body is well under 1 MiB; 16 MiB gives headroom for batch
/// writes without letting a malformed client OOM us.
const MAX_DATA_LEN: u32 = 16 * 1024 * 1024;

pub struct FastCodec;

impl Decoder for FastCodec {
    type Item = FastMessage;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<FastMessage>, CodecError> {
        if src.len() < FP_HEADER_SZ {
            return Ok(None);
        }
        let version = src[0];
        if version != 1 && version != 2 {
            return Err(CodecError::UnsupportedVersion(version));
        }
        let msg_type = src[1];
        if msg_type != FP_TYPE_JSON {
            return Err(CodecError::UnsupportedType(msg_type));
        }
        let status = FastStatus::from_u8(src[2])
            .ok_or(CodecError::UnknownStatus(src[2]))?;
        let msgid = (&src[3..7]).get_u32();
        let _crc = (&src[7..11]).get_u32(); // accepted as-is; see module docs
        let data_len = (&src[11..15]).get_u32();
        if data_len > MAX_DATA_LEN {
            return Err(CodecError::Overflow(data_len));
        }
        let total = FP_HEADER_SZ + data_len as usize;
        if src.len() < total {
            src.reserve(total - src.len());
            return Ok(None);
        }
        src.advance(FP_HEADER_SZ);
        let body = src.split_to(data_len as usize);
        let data: FastData = serde_json::from_slice(&body)?;
        Ok(Some(FastMessage { id: msgid, status, data }))
    }
}

impl Encoder<FastMessage> for FastCodec {
    type Error = CodecError;

    fn encode(&mut self, msg: FastMessage, dst: &mut BytesMut) -> Result<(), CodecError> {
        // Encode as pure-ASCII JSON: any non-ASCII code point goes out
        // as a `\uXXXX` escape. This is valid JSON (clients decode back
        // to the same string) AND it keeps the body byte-identical to
        // what the old `node-crc@0.3.0` library walks through on the
        // client side. That library CRCs each JS code point as a
        // 16-bit value rather than the UTF-8 bytes, so a raw
        // multi-byte UTF-8 sequence would produce a different CRC than
        // our byte-level XMODEM — causing the `fast@2.8.2` client to
        // fail the message. Escaping flattens that difference: pure
        // ASCII bytes → identical code points → identical CRC.
        let raw = serde_json::to_vec(&msg.data)?;
        let body = ascii_escape_json(&raw);
        let data_len: u32 = body.len().try_into().map_err(|_| {
            CodecError::Overflow(u32::MAX)
        })?;
        if data_len > MAX_DATA_LEN {
            return Err(CodecError::Overflow(data_len));
        }
        let crc = crc16_ccitt_xmodem(&body) as u32;

        dst.reserve(FP_HEADER_SZ + body.len());
        dst.put_u8(FP_VERSION_CURRENT);
        dst.put_u8(FP_TYPE_JSON);
        dst.put_u8(msg.status.as_u8());
        dst.put_u32(msg.id);
        dst.put_u32(crc);
        dst.put_u32(data_len);
        dst.extend_from_slice(&body);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn roundtrip() {
        let mut codec = FastCodec;
        let msg = FastMessage::data(
            42,
            "ping",
            serde_json::json!([{"hello": "world"}]),
        );
        let mut buf = BytesMut::new();
        codec.encode(msg.clone(), &mut buf).unwrap();
        let out = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(out.id, 42);
        assert_eq!(out.status, FastStatus::Data);
        assert_eq!(out.data.m.name, "ping");
    }

    #[test]
    fn short_frame_returns_none() {
        let mut codec = FastCodec;
        let mut buf = BytesMut::from(&[2u8, 1, 1, 0, 0, 0, 1][..]); // 7 bytes
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }
}
