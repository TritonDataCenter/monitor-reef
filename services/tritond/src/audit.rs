// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Audit emission glue between the request lifecycle and
//! [`tritond_audit::Chain`].
//!
//! The emission shape is:
//!
//! * **Cedar decisions.** Every [`crate::auth::authenticate_and_authorize`]
//!   call records exactly one event after the Cedar verdict. Allow
//!   events always log; deny events log only for *authenticated*
//!   callers. Anonymous denies (probe noise) are intentionally not
//!   logged — they would otherwise dominate the chain.
//! * **Mutations.** Handlers that change persistent state call
//!   [`AuditService::record_mutation`] after the write succeeds (or
//!   fails) so the resource id and outcome are captured.
//! * **Auth lifecycle.** `/v2/auth/login` and `/v2/auth/refresh`
//!   emit events with the username + outcome (success vs reason for
//!   failure) so credential-stuffing patterns are visible to operators.
//!
//! Each `record_*` call is one append on the underlying [`Chain`]
//! (one FDB transaction in the production backend). Failures to
//! append are logged at `warn!` and not propagated — the request
//! must succeed even if audit is briefly unavailable. (When the
//! manta-storage substrate lands, the audit-emit path becomes a
//! best-effort fan-out with retries; the trade-off here is identical.)

use std::sync::Arc;

use chrono::Utc;
use tracing::warn;
use tritond_audit::{Actor, AuditEvent, Chain, Decision, Outcome, PendingEvent};
use uuid::Uuid;

use crate::auth::{Action, Principal};

/// Per-cluster audit service. Owns an [`Arc<dyn Chain>`] handle used
/// by every emission point.
pub struct AuditService {
    chain: Arc<dyn Chain>,
}

impl AuditService {
    pub fn new(chain: Arc<dyn Chain>) -> Self {
        Self { chain }
    }

    pub fn chain(&self) -> &Arc<dyn Chain> {
        &self.chain
    }

    /// Emit a Cedar decision event. Returns the materialised event
    /// for tests; production callers can ignore it.
    ///
    /// Anonymous deny events are skipped to keep the chain free of
    /// probe noise — see module-level comment.
    pub async fn record_decision(
        &self,
        principal: &Principal,
        action: Action,
        request_id: Option<Uuid>,
        decision: Decision,
    ) -> Option<AuditEvent> {
        if matches!(decision, Decision::Deny) && matches!(principal, Principal::Anonymous) {
            return None;
        }
        let outcome = match decision {
            Decision::Allow => Outcome::Success { resource: None },
            Decision::Deny => Outcome::Forbidden,
            // NotEvaluated and any future variant: shaped as Success
            // with no resource. The decision field on the event still
            // carries the precise variant for downstream readers.
            _ => Outcome::Success { resource: None },
        };
        self.append(PendingEvent {
            ts: Utc::now(),
            actor: principal_to_actor(principal),
            action: action.cedar_id().to_string(),
            resource: None,
            request_id,
            decision,
            outcome,
            payload: serde_json::Value::Null,
        })
        .await
    }

    /// Emit a mutation-outcome event after a handler has finished
    /// the persistent write. `resource` is the entity-uid form
    /// (`Silo::"<uuid>"` etc) when known.
    pub async fn record_mutation(
        &self,
        principal: &Principal,
        action: Action,
        request_id: Option<Uuid>,
        resource: Option<String>,
        outcome: Outcome,
        payload: serde_json::Value,
    ) -> Option<AuditEvent> {
        self.append(PendingEvent {
            ts: Utc::now(),
            actor: principal_to_actor(principal),
            action: action.cedar_id().to_string(),
            resource,
            request_id,
            decision: Decision::Allow,
            outcome,
            payload,
        })
        .await
    }

    /// Emit an auth-lifecycle event (login, refresh). `username` is
    /// always logged (denies and successes alike); the password is
    /// never in scope here.
    pub async fn record_auth_event(
        &self,
        action: Action,
        username: &str,
        request_id: Option<Uuid>,
        actor: Actor,
        outcome: Outcome,
    ) -> Option<AuditEvent> {
        let payload = serde_json::json!({ "username": username });
        let decision = match outcome {
            Outcome::Success { .. } => Decision::Allow,
            Outcome::Unauthenticated { .. } => Decision::Deny,
            _ => Decision::NotEvaluated,
        };
        self.append(PendingEvent {
            ts: Utc::now(),
            actor,
            action: action.cedar_id().to_string(),
            resource: None,
            request_id,
            decision,
            outcome,
            payload,
        })
        .await
    }

    async fn append(&self, pending: PendingEvent) -> Option<AuditEvent> {
        match self.chain.append(pending).await {
            Ok(event) => Some(event),
            Err(e) => {
                warn!(error = %e, "audit append failed; request will continue");
                None
            }
        }
    }
}

fn principal_to_actor(principal: &Principal) -> Actor {
    match principal {
        Principal::Operator {
            user_id, is_root, ..
        } => Actor::Operator {
            user_id: *user_id,
            is_root: *is_root,
        },
        Principal::Anonymous => Actor::Anonymous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tritond_audit::MemChain;

    fn fresh() -> AuditService {
        AuditService::new(Arc::new(MemChain::new()))
    }

    #[tokio::test]
    async fn anonymous_deny_does_not_emit() {
        let svc = fresh();
        let emitted = svc
            .record_decision(
                &Principal::Anonymous,
                Action::CreateSilo,
                None,
                Decision::Deny,
            )
            .await;
        assert!(emitted.is_none());
        assert!(svc.chain.head().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn authenticated_deny_does_emit() {
        let svc = fresh();
        let principal = Principal::Operator {
            user_id: Uuid::new_v4(),
            is_root: false,
            silo_id: None,
        };
        let emitted = svc
            .record_decision(&principal, Action::CreateSilo, None, Decision::Deny)
            .await;
        assert!(emitted.is_some());
        assert_eq!(svc.chain.head().await.unwrap().map(|h| h.seq), Some(0));
    }

    #[tokio::test]
    async fn allow_decision_emits_for_anonymous_too() {
        let svc = fresh();
        let emitted = svc
            .record_decision(&Principal::Anonymous, Action::Login, None, Decision::Allow)
            .await;
        assert!(emitted.is_some());
    }

    #[tokio::test]
    async fn auth_event_logs_username_in_payload() {
        let svc = fresh();
        let emitted = svc
            .record_auth_event(
                Action::Login,
                "root",
                None,
                Actor::Anonymous,
                Outcome::Success { resource: None },
            )
            .await
            .unwrap();
        assert_eq!(emitted.payload["username"], "root");
    }
}
