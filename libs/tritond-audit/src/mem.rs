// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-memory [`Chain`] backed by a `Vec` behind `tokio::sync::RwLock`.
//!
//! Used for unit tests, integration tests, and `tritond` runs that
//! don't need durable audit state.

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::{AuditError, AuditEvent, Chain, ChainHead, EventHash, PendingEvent, VerifyOutcome};

/// In-process [`Chain`] implementation.
pub struct MemChain {
    events: RwLock<Vec<AuditEvent>>,
}

impl MemChain {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }
}

impl Default for MemChain {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Chain for MemChain {
    async fn append(&self, pending: PendingEvent) -> Result<AuditEvent, AuditError> {
        let mut guard = self.events.write().await;
        let (seq, prev_hash) = match guard.last() {
            Some(prev) => (prev.seq + 1, prev.hash.clone()),
            None => (0, EventHash::zero()),
        };
        let event = AuditEvent::from_pending(pending, seq, prev_hash)?;
        guard.push(event.clone());
        Ok(event)
    }

    async fn get(&self, seq: u64) -> Result<AuditEvent, AuditError> {
        let guard = self.events.read().await;
        let head_seq = guard.last().map_or(0u64, |e| e.seq);
        if guard.is_empty() {
            return Err(AuditError::PastHead { seq, head: 0 });
        }
        if seq > head_seq {
            return Err(AuditError::PastHead {
                seq,
                head: head_seq,
            });
        }
        // Vec index == seq because we always start at 0 and never gap.
        let idx = usize::try_from(seq)
            .map_err(|_| AuditError::Backend(format!("seq {seq} doesn't fit in usize")))?;
        guard.get(idx).cloned().ok_or(AuditError::PastHead {
            seq,
            head: head_seq,
        })
    }

    async fn list(&self, after_seq: u64, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
        let guard = self.events.read().await;
        Ok(guard
            .iter()
            .filter(|e| e.seq > after_seq)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn head(&self) -> Result<Option<ChainHead>, AuditError> {
        let guard = self.events.read().await;
        Ok(guard.last().map(|e| ChainHead {
            seq: e.seq,
            hash: e.hash.clone(),
        }))
    }

    async fn verify(&self, from: u64, to: u64) -> Result<VerifyOutcome, AuditError> {
        let guard = self.events.read().await;
        if guard.is_empty() {
            return Ok(VerifyOutcome::Ok { verified_to: 0 });
        }
        let head_seq = guard.last().map_or(0, |e| e.seq);
        let to = to.min(head_seq);
        if from > to {
            return Ok(VerifyOutcome::Ok {
                verified_to: head_seq,
            });
        }

        // Pull the prior event (if any) so we can check the start
        // of the range chains correctly.
        let mut prior_hash = if from == 0 {
            EventHash::zero()
        } else {
            let idx = usize::try_from(from - 1)
                .map_err(|_| AuditError::Backend("from-1 doesn't fit in usize".to_string()))?;
            guard
                .get(idx)
                .map(|e| e.hash.clone())
                .ok_or(AuditError::PastHead {
                    seq: from - 1,
                    head: head_seq,
                })?
        };

        let from_idx = usize::try_from(from)
            .map_err(|_| AuditError::Backend("from doesn't fit in usize".to_string()))?;
        let to_idx = usize::try_from(to)
            .map_err(|_| AuditError::Backend("to doesn't fit in usize".to_string()))?;

        for event in &guard[from_idx..=to_idx] {
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
        }
        Ok(VerifyOutcome::Ok { verified_to: to })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Actor, Decision, Outcome};
    use chrono::Utc;

    fn pending(action: &str) -> PendingEvent {
        PendingEvent {
            ts: Utc::now(),
            actor: Actor::System,
            action: action.to_string(),
            resource: None,
            request_id: None,
            decision: Decision::Allow,
            outcome: Outcome::Success { resource: None },
            payload: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn appended_events_chain_correctly() {
        let chain = MemChain::new();
        let a = chain.append(pending("a")).await.unwrap();
        let b = chain.append(pending("b")).await.unwrap();
        let c = chain.append(pending("c")).await.unwrap();
        assert_eq!(a.seq, 0);
        assert_eq!(b.seq, 1);
        assert_eq!(c.seq, 2);
        assert_eq!(a.prev_hash, EventHash::zero());
        assert_eq!(b.prev_hash, a.hash);
        assert_eq!(c.prev_hash, b.hash);
    }

    #[tokio::test]
    async fn empty_chain_has_no_head() {
        let chain = MemChain::new();
        assert!(chain.head().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_pages_after_seq() {
        let chain = MemChain::new();
        for i in 0..5 {
            chain.append(pending(&format!("a{i}"))).await.unwrap();
        }
        let listed = chain.list(1, 10).await.unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].seq, 2);

        let limited = chain.list(0, 2).await.unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].seq, 1);
        assert_eq!(limited[1].seq, 2);
    }

    #[tokio::test]
    async fn verify_clean_chain_is_ok() {
        let chain = MemChain::new();
        for i in 0..3 {
            chain.append(pending(&format!("e{i}"))).await.unwrap();
        }
        let outcome = chain.verify(0, 2).await.unwrap();
        assert!(matches!(outcome, VerifyOutcome::Ok { verified_to: 2 }));
    }

    #[tokio::test]
    async fn verify_detects_tampered_event() {
        let chain = MemChain::new();
        for i in 0..3 {
            chain.append(pending(&format!("e{i}"))).await.unwrap();
        }
        // Tamper with seq=1 in place.
        {
            let mut guard = chain.events.write().await;
            guard[1].action = "tampered".to_string();
        }
        let outcome = chain.verify(0, 2).await.unwrap();
        match outcome {
            VerifyOutcome::Mismatch { seq, .. } => {
                // The hash at seq=1 no longer matches its content; this
                // shows up either as a hash mismatch at seq=1 or as a
                // prev_hash mismatch at seq=2 depending on order.
                assert!(seq == 1 || seq == 2);
            }
            VerifyOutcome::Ok { .. } => panic!("expected mismatch"),
        }
    }

    #[tokio::test]
    async fn get_unknown_seq_returns_past_head() {
        let chain = MemChain::new();
        chain.append(pending("a")).await.unwrap();
        let err = chain.get(99).await.expect_err("must be past head");
        assert!(matches!(err, AuditError::PastHead { .. }));
    }
}
