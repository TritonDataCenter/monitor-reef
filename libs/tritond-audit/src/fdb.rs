// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FoundationDB-backed [`Chain`].
//!
//! Compiled in only when the `foundationdb` cargo feature is enabled
//! (same shape as `tritond-store::FdbStore`). Sharing the FDB binding
//! means we share the boot-once `NetworkAutoStop` guard with the
//! storage layer; using the same `Database` handle is the caller's
//! responsibility.
//!
//! # Schema
//!
//! ```text
//! audit/operator/seq/<u64-bigendian>      -> JSON-encoded AuditEvent
//! audit/operator/by_request/<request_id>  -> u64 seq (be) cross-link
//! audit/operator/head                     -> JSON ChainHead
//! ```
//!
//! Each `append` is a single FDB transaction that reads the head,
//! computes prev_hash + hash, writes the seq key + by_request index +
//! advances head. Concurrent appends conflict and FDB retries the
//! closure with the new head.
//!
//! # Migration
//!
//! When manta-storage Phase 0/1 ships, this module's call sites move
//! to a `MantaSubstrateChain` that writes to manta-storage's
//! `("e", region, tenant_shard, versionstamp, ...)` events keyspace
//! and tiers sealed segments to manta-s3 with Object Lock. Today's
//! events translate cleanly: the JSON wire form survives unchanged.

use std::sync::Arc;

use async_trait::async_trait;
use foundationdb::{Database, FdbBindingError, KeySelector, RangeOption};

use crate::{AuditError, AuditEvent, Chain, ChainHead, EventHash, PendingEvent, VerifyOutcome};

const SEQ_PREFIX: &[u8] = b"audit/operator/seq/";
const BY_REQUEST_PREFIX: &[u8] = b"audit/operator/by_request/";
const HEAD_KEY: &[u8] = b"audit/operator/head";

/// FDB-backed [`Chain`].
pub struct FdbChain {
    db: Arc<Database>,
}

impl FdbChain {
    /// Wrap a shared FDB database. The caller owns the boot lifecycle
    /// (typically via `tritond_store::FdbStore::open`); we share the
    /// handle via `Arc` because `foundationdb::Database` is not
    /// `Clone`.
    #[must_use]
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn seq_key(seq: u64) -> Vec<u8> {
        let mut k = SEQ_PREFIX.to_vec();
        k.extend_from_slice(&seq.to_be_bytes());
        k
    }

    fn by_request_key(request_id_bytes: &[u8]) -> Vec<u8> {
        let mut k = BY_REQUEST_PREFIX.to_vec();
        k.extend_from_slice(request_id_bytes);
        k
    }
}

/// Compute the half-open range `[prefix, prefix++)` for prefix scans.
fn prefix_range(prefix: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut end = prefix.to_vec();
    for byte in end.iter_mut().rev() {
        if *byte < 0xFF {
            *byte += 1;
            return (prefix.to_vec(), end);
        }
        *byte = 0;
    }
    end.push(0);
    (prefix.to_vec(), end)
}

#[async_trait]
impl Chain for FdbChain {
    async fn append(&self, pending: PendingEvent) -> Result<AuditEvent, AuditError> {
        // Pre-serialise the request_id key once; the closure may run
        // multiple times if FDB retries, but request_id is stable.
        let request_id_bytes = pending.request_id.map(|u| u.as_bytes().to_vec());
        let pending_for_closure = pending.clone();

        let result: Result<AuditEvent, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let pending = pending_for_closure.clone();
                let request_id_bytes = request_id_bytes.clone();
                async move {
                    let head_bytes = tr.get(HEAD_KEY, false).await?;
                    let (seq, prev_hash) = match head_bytes {
                        Some(bytes) => {
                            let head: ChainHead = serde_json::from_slice(&bytes).map_err(|e| {
                                FdbBindingError::CustomError(Box::new(SerialiseErr(format!(
                                    "decode head: {e}"
                                ))))
                            })?;
                            (head.seq + 1, head.hash)
                        }
                        None => (0, EventHash::zero()),
                    };
                    let event = AuditEvent::from_pending(pending, seq, prev_hash).map_err(|e| {
                        FdbBindingError::CustomError(Box::new(SerialiseErr(e.to_string())))
                    })?;
                    let event_bytes = serde_json::to_vec(&event).map_err(|e| {
                        FdbBindingError::CustomError(Box::new(SerialiseErr(format!(
                            "encode event: {e}"
                        ))))
                    })?;
                    let new_head = ChainHead {
                        seq: event.seq,
                        hash: event.hash.clone(),
                    };
                    let new_head_bytes = serde_json::to_vec(&new_head).map_err(|e| {
                        FdbBindingError::CustomError(Box::new(SerialiseErr(format!(
                            "encode head: {e}"
                        ))))
                    })?;
                    tr.set(&FdbChain::seq_key(seq), &event_bytes);
                    if let Some(rid) = request_id_bytes.as_deref() {
                        tr.set(&FdbChain::by_request_key(rid), &seq.to_be_bytes());
                    }
                    tr.set(HEAD_KEY, &new_head_bytes);
                    Ok(event)
                }
            })
            .await;

        result.map_err(|e| match e {
            FdbBindingError::CustomError(inner) => match inner.downcast::<SerialiseErr>() {
                Ok(serr) => AuditError::Serialise(serr.0),
                Err(other) => AuditError::Backend(format!("FDB transaction: {other:?}")),
            },
            other => AuditError::Backend(format!("FDB transaction: {other}")),
        })
    }

    async fn get(&self, seq: u64) -> Result<AuditEvent, AuditError> {
        let key = Self::seq_key(seq);
        let bytes_result: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;
        let bytes =
            bytes_result.map_err(|e| AuditError::Backend(format!("FDB transaction: {e}")))?;
        match bytes {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| AuditError::Serialise(e.to_string()))
            }
            None => {
                let head_seq = match self.head().await? {
                    Some(h) => h.seq,
                    None => 0,
                };
                Err(AuditError::PastHead {
                    seq,
                    head: head_seq,
                })
            }
        }
    }

    async fn list(&self, after_seq: u64, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
        let begin_key = Self::seq_key(after_seq.saturating_add(1));
        let (_, end_excl) = prefix_range(SEQ_PREFIX);

        let result: Result<Vec<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let begin = begin_key.clone();
                let end = end_excl.clone();
                async move {
                    let opt = RangeOption {
                        begin: KeySelector::first_greater_or_equal(begin),
                        end: KeySelector::first_greater_or_equal(end),
                        limit: Some(limit),
                        ..RangeOption::default()
                    };
                    let kvs = tr.get_range(&opt, 1, false).await?;
                    Ok(kvs.iter().map(|kv| kv.value().to_vec()).collect())
                }
            })
            .await;
        let raws = result.map_err(|e| AuditError::Backend(format!("FDB transaction: {e}")))?;
        let mut out = Vec::with_capacity(raws.len());
        for bytes in raws {
            let event: AuditEvent =
                serde_json::from_slice(&bytes).map_err(|e| AuditError::Serialise(e.to_string()))?;
            out.push(event);
        }
        Ok(out)
    }

    async fn head(&self) -> Result<Option<ChainHead>, AuditError> {
        let result: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| async move { Ok(tr.get(HEAD_KEY, false).await?.map(|s| s.to_vec())) })
            .await;
        let bytes = result.map_err(|e| AuditError::Backend(format!("FDB transaction: {e}")))?;
        match bytes {
            Some(bytes) => {
                let head: ChainHead = serde_json::from_slice(&bytes)
                    .map_err(|e| AuditError::Serialise(e.to_string()))?;
                Ok(Some(head))
            }
            None => Ok(None),
        }
    }

    async fn verify(&self, from: u64, to: u64) -> Result<VerifyOutcome, AuditError> {
        let head = self.head().await?;
        let head_seq = head.as_ref().map_or(0u64, |h| h.seq);
        if head.is_none() {
            return Ok(VerifyOutcome::Ok { verified_to: 0 });
        }
        let to = to.min(head_seq);
        if from > to {
            return Ok(VerifyOutcome::Ok {
                verified_to: head_seq,
            });
        }

        let mut prior_hash = if from == 0 {
            EventHash::zero()
        } else {
            self.get(from - 1).await?.hash
        };

        let mut cursor = from;
        while cursor <= to {
            let event = self.get(cursor).await?;
            if event.prev_hash != prior_hash {
                return Ok(VerifyOutcome::Mismatch {
                    seq: event.seq,
                    message: format!(
                        "prev_hash {} did not match prior hash {}",
                        event.prev_hash.0, prior_hash.0
                    ),
                });
            }
            let recomputed = event.compute_hash()?;
            if event.hash != recomputed {
                return Ok(VerifyOutcome::Mismatch {
                    seq: event.seq,
                    message: format!(
                        "stored hash {} did not match recomputed {}",
                        event.hash.0, recomputed.0
                    ),
                });
            }
            prior_hash = event.hash.clone();
            cursor += 1;
        }
        Ok(VerifyOutcome::Ok { verified_to: to })
    }
}

/// Wrapper so we can carry a serialise error out of an FDB closure
/// via `FdbBindingError::CustomError`.
#[derive(Debug)]
struct SerialiseErr(String);

impl std::fmt::Display for SerialiseErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SerialiseErr {}
