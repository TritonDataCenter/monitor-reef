// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Wire types for audit events plus hash-chain helpers.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// SHA-256 chain link, hex-encoded for transport. Stored as a
/// fixed-length 64-character lowercase hex string so the wire format
/// is human-readable and serde-symmetric without specialised codecs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct EventHash(pub String);

impl EventHash {
    /// The all-zero hash, used as `prev_hash` of the genesis event.
    #[must_use]
    pub fn zero() -> Self {
        Self("0".repeat(64))
    }

    /// Hash the canonical-JSON form of the event body (every field
    /// of `AuditEvent` except `hash` itself).
    fn from_canonical(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(hex::encode(digest))
    }
}

/// Who initiated the action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Actor {
    /// Authenticated operator with an `is_root` flag captured at
    /// the time of the event.
    Operator { user_id: Uuid, is_root: bool },
    /// Authenticated via API key. `key_id` lets auditors trace the
    /// credential without revealing it.
    ApiKey { key_id: Uuid, user_id: Uuid },
    /// No credential presented. Used only for events where logging
    /// the unauthenticated principal is intentional (e.g. login
    /// attempts; not for routine deny-on-anonymous probes).
    Anonymous,
    /// Internal system action (bootstrap, cron, replication).
    System,
}

/// Cedar decision recorded with the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Decision {
    /// Cedar permitted the action.
    Allow,
    /// Cedar denied the action.
    Deny,
    /// Decision was not evaluated (e.g. recorded before Cedar runs).
    NotEvaluated,
}

/// Outcome of the action after the handler finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    /// Handler completed successfully. `resource` carries the
    /// affected entity uid when known (e.g. `Silo::"<uuid>"`).
    Success { resource: Option<String> },
    /// Cedar denied; handler did not run.
    Forbidden,
    /// Authentication failed (bad password, expired token, missing key).
    Unauthenticated { reason: String },
    /// Caller-side error (4xx other than 401/403).
    ClientError { code: u16, message: String },
    /// Server-side error (5xx).
    ServerError { message: String },
}

/// Event ready to append: every field except those the chain
/// assigns (`seq`, `prev_hash`, `hash`).
#[derive(Debug, Clone)]
pub struct PendingEvent {
    pub ts: DateTime<Utc>,
    pub actor: Actor,
    pub action: String,
    pub resource: Option<String>,
    pub request_id: Option<Uuid>,
    pub decision: Decision,
    pub outcome: Outcome,
    pub payload: serde_json::Value,
}

/// Materialised audit event. The `hash` is `SHA-256(canonical-JSON of
/// every field except hash itself)` so a verifier reading any prefix
/// of the chain can recompute and compare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEvent {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub actor: Actor,
    pub action: String,
    pub resource: Option<String>,
    pub request_id: Option<Uuid>,
    pub decision: Decision,
    pub outcome: Outcome,
    pub payload: serde_json::Value,
    pub prev_hash: EventHash,
    pub hash: EventHash,
}

impl AuditEvent {
    /// Build the next event in a chain from a [`PendingEvent`] plus
    /// the previous event's seq + hash. Genesis event uses
    /// [`EventHash::zero`] and `seq = 0`. Returns an error only if
    /// the payload contains a JSON shape that can't be serialised
    /// (in practice impossible for our event types, but the
    /// [`crate::AuditError::Serialise`] variant exists so callers
    /// surface rather than panic).
    pub fn from_pending(
        pending: PendingEvent,
        seq: u64,
        prev_hash: EventHash,
    ) -> Result<Self, super::AuditError> {
        let mut event = AuditEvent {
            seq,
            ts: pending.ts,
            actor: pending.actor,
            action: pending.action,
            resource: pending.resource,
            request_id: pending.request_id,
            decision: pending.decision,
            outcome: pending.outcome,
            payload: pending.payload,
            prev_hash,
            hash: EventHash::zero(),
        };
        event.hash = event.compute_hash()?;
        Ok(event)
    }

    /// Recompute this event's hash from its non-hash fields. Used by
    /// [`crate::Chain::verify`].
    pub fn compute_hash(&self) -> Result<EventHash, super::AuditError> {
        let canonical = self.canonical_bytes_for_hash()?;
        Ok(EventHash::from_canonical(&canonical))
    }

    fn canonical_bytes_for_hash(&self) -> Result<Vec<u8>, super::AuditError> {
        // The intermediate `Value` dance is needed because the struct
        // has a `hash` field we must not include in its own preimage.
        let intermediate = serde_json::json!({
            "seq": self.seq,
            "ts": self.ts,
            "actor": self.actor,
            "action": self.action,
            "resource": self.resource,
            "request_id": self.request_id,
            "decision": self.decision,
            "outcome": self.outcome,
            "payload": self.payload,
            "prev_hash": self.prev_hash,
        });
        serde_json::to_vec(&intermediate).map_err(|e| super::AuditError::Serialise(e.to_string()))
    }
}

/// Snapshot of where the chain currently ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChainHead {
    /// Sequence number of the most recently appended event.
    pub seq: u64,
    /// Its `hash` — the value the next append will use as `prev_hash`.
    pub hash: EventHash,
}

/// Result of [`crate::Chain::verify`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum VerifyOutcome {
    /// The walked range is internally consistent and chains back to
    /// `from - 1` correctly. `verified_to` is the highest seq reached.
    Ok { verified_to: u64 },
    /// Found a hash mismatch. `seq` is the first event whose stored
    /// hash does not match its recomputed value, or whose `prev_hash`
    /// does not match the prior event's `hash`. The chain bytes
    /// themselves remain queryable; the operator decides what to do.
    Mismatch { seq: u64, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(action: &str) -> PendingEvent {
        PendingEvent {
            ts: chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("fixed valid timestamp"),
            actor: Actor::System,
            action: action.to_string(),
            resource: None,
            request_id: None,
            decision: Decision::Allow,
            outcome: Outcome::Success { resource: None },
            payload: serde_json::Value::Null,
        }
    }

    #[test]
    fn from_pending_seeds_genesis_with_zero_prev_hash() {
        let ev = AuditEvent::from_pending(pending("genesis"), 0, EventHash::zero()).unwrap();
        assert_eq!(ev.seq, 0);
        assert_eq!(ev.prev_hash, EventHash::zero());
        assert_eq!(ev.hash, ev.compute_hash().unwrap());
    }

    #[test]
    fn changing_a_field_changes_the_hash() {
        let ev = AuditEvent::from_pending(pending("a"), 0, EventHash::zero()).unwrap();
        let mut tampered = ev.clone();
        tampered.action = "b".to_string();
        assert_ne!(ev.compute_hash().unwrap(), tampered.compute_hash().unwrap());
    }

    #[test]
    fn hash_is_deterministic_across_constructions() {
        let a = AuditEvent::from_pending(pending("same"), 7, EventHash::zero()).unwrap();
        let b = AuditEvent::from_pending(pending("same"), 7, EventHash::zero()).unwrap();
        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn hash_string_is_64_lowercase_hex() {
        let ev = AuditEvent::from_pending(pending("x"), 0, EventHash::zero()).unwrap();
        assert_eq!(ev.hash.0.len(), 64);
        assert!(
            ev.hash
                .0
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
