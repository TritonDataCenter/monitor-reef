// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! [`StatusSink`] wiring the harvested
//! [`tritond_cn_platform::cn_status::Heartbeater`] to tritond's
//! `POST /v2/agent/heartbeat` and `POST /v2/agent/status`.
//!
//! The implementation is intentionally a thin adapter: the legacy
//! transport rules (auth, base URL, TLS posture) all live in the
//! [`tritond_client::Client`] this sink is constructed with -- so
//! the same authenticated client used for `agent_claim_job` is
//! reused here, and no separate credential plumbing is needed.

use std::sync::Arc;

use async_trait::async_trait;
use tritond_client::Client;
use tritond_client::types::AgentStatusRequest;
use tritond_cn_platform::cn_status::{StatusSink, StatusSinkError};

/// [`StatusSink`] backed by an authenticated [`tritond_client::Client`].
///
/// Cheap to clone; the inner [`Client`] is reused by the heartbeater for
/// both the lightweight ping and the full status post.
#[derive(Clone)]
pub struct TritondStatusSink {
    client: Arc<Client>,
}

impl TritondStatusSink {
    /// Wrap an authenticated [`Client`] for use as a [`StatusSink`].
    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl StatusSink for TritondStatusSink {
    async fn post_heartbeat(&self) -> Result<(), StatusSinkError> {
        self.client
            .agent_heartbeat()
            .send()
            .await
            .map(|_| ())
            .map_err(|e| StatusSinkError::Transport(e.to_string()))
    }

    async fn post_status(&self, body: &serde_json::Value) -> Result<(), StatusSinkError> {
        self.client
            .agent_status()
            .body(AgentStatusRequest {
                payload: body.clone(),
            })
            .send()
            .await
            .map(|_| ())
            .map_err(|e| StatusSinkError::Transport(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time guarantee that [`TritondStatusSink`] satisfies the
    /// [`StatusSink`] trait the heartbeater expects. If the heartbeater
    /// changes shape this stops compiling at the same site that needs
    /// fixing.
    #[allow(dead_code)]
    fn _assert_sink<T: StatusSink>() {}

    #[test]
    fn sink_satisfies_status_sink_trait() {
        _assert_sink::<TritondStatusSink>();
    }

    #[test]
    fn sink_constructs_from_arc_client() {
        // The client doesn't need to be reachable for construction; this
        // exercises the Arc<Client> path so a future Client API change
        // (e.g. losing `new`) breaks here, not in production wiring.
        let client = Arc::new(Client::new("http://127.0.0.1:1"));
        let _sink = TritondStatusSink::new(client);
    }
}
