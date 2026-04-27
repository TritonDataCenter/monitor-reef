// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-disk value framing for stored `ObjectMeta` / `Bucket` records.
//!
//! FDB caps each value at 100,000 bytes. JSON-serialised object bodies
//! in real Triton traffic (`wf_jobs` rows carrying serialised workflow
//! state) routinely exceed that. We sidestep the cap by compressing
//! with zstd, which brings 100 KB JSON down to 10–30 KB in practice.
//!
//! Wire format is one-byte discriminator + payload:
//!
//! - `0x4A` (`'J'`) → raw JSON (back-compat for values written before
//!   compression was introduced).
//! - `0x5A` (`'Z'`) → zstd-compressed JSON.
//!
//! Anything that doesn't match either tag is assumed to be raw JSON so
//! we never accidentally corrupt an older database on upgrade.

use crate::error::{MorayError, Result};

const TAG_RAW_JSON: u8 = b'J';
const TAG_ZSTD: u8 = b'Z';
/// Keep individual post-compression values well under FDB's 100 KB
/// cap. Headroom for future envelope fields.
const FDB_VALUE_CEILING: usize = 95_000;
/// Compression level: 3 is zstd's default and balances ratio vs. CPU.
const ZSTD_LEVEL: i32 = 3;

pub fn encode<T: serde::Serialize>(v: &T) -> Result<Vec<u8>> {
    let raw = serde_json::to_vec(v).map_err(MorayError::Serde)?;
    // Tiny payloads skip compression — overhead isn't worth it for the
    // common small-object case.
    if raw.len() < 512 {
        let mut out = Vec::with_capacity(raw.len() + 1);
        out.push(TAG_RAW_JSON);
        out.extend_from_slice(&raw);
        return Ok(out);
    }
    let compressed = zstd::encode_all(&raw[..], ZSTD_LEVEL).map_err(|e| {
        MorayError::Storage(anyhow::anyhow!("zstd encode: {e}"))
    })?;
    // If zstd somehow inflates (rare, but possible on already-
    // compressed payloads), keep the raw form.
    if compressed.len() + 1 >= raw.len() + 1 {
        let mut out = Vec::with_capacity(raw.len() + 1);
        out.push(TAG_RAW_JSON);
        out.extend_from_slice(&raw);
        if out.len() > FDB_VALUE_CEILING {
            return Err(MorayError::Storage(anyhow::anyhow!(
                "encoded value {} bytes exceeds FDB per-value ceiling ({})",
                out.len(),
                FDB_VALUE_CEILING
            )));
        }
        return Ok(out);
    }
    let mut out = Vec::with_capacity(compressed.len() + 1);
    out.push(TAG_ZSTD);
    out.extend_from_slice(&compressed);
    if out.len() > FDB_VALUE_CEILING {
        return Err(MorayError::Storage(anyhow::anyhow!(
            "compressed value {} bytes still exceeds FDB ceiling ({})",
            out.len(),
            FDB_VALUE_CEILING
        )));
    }
    Ok(out)
}

pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.is_empty() {
        return Err(MorayError::Invariant("empty encoded value".into()));
    }
    match bytes[0] {
        TAG_ZSTD => {
            let raw = zstd::decode_all(&bytes[1..]).map_err(|e| {
                MorayError::Storage(anyhow::anyhow!("zstd decode: {e}"))
            })?;
            serde_json::from_slice(&raw).map_err(MorayError::Serde)
        }
        TAG_RAW_JSON => serde_json::from_slice(&bytes[1..]).map_err(MorayError::Serde),
        // Back-compat: anything else we assume is legacy uncompressed
        // JSON written before the framing byte was introduced.
        _ => serde_json::from_slice(bytes).map_err(MorayError::Serde),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Blob {
        key: String,
        payload: String,
    }

    #[test]
    fn round_trip_small() {
        let b = Blob { key: "k".into(), payload: "hi".into() };
        let enc = encode(&b).unwrap();
        assert_eq!(enc[0], TAG_RAW_JSON);
        let dec: Blob = decode(&enc).unwrap();
        assert_eq!(b, dec);
    }

    #[test]
    fn round_trip_large_gets_compressed() {
        let b = Blob {
            key: "big".into(),
            payload: "x".repeat(200_000),
        };
        let enc = encode(&b).unwrap();
        assert_eq!(enc[0], TAG_ZSTD);
        assert!(enc.len() < 5_000, "zstd should squash 200k x's small");
        let dec: Blob = decode(&enc).unwrap();
        assert_eq!(b, dec);
    }

    #[test]
    fn legacy_untagged_json_still_decodes() {
        let raw = serde_json::to_vec(&Blob {
            key: "old".into(),
            payload: "data".into(),
        })
        .unwrap();
        let dec: Blob = decode(&raw).unwrap();
        assert_eq!(dec.payload, "data");
    }
}
