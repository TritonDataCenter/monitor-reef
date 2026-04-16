// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared request-handler state.

use std::collections::VecDeque;
use std::sync::Mutex;

use cn_agent_api::{TaskHistoryEntry, Uuid};

use crate::TASK_HISTORY_SIZE;
use crate::registry::TaskRegistry;

/// Metadata surfaced on `/ping` and used for logging.
#[derive(Debug, Clone)]
pub struct AgentMetadata {
    pub name: String,
    pub version: String,
    pub server_uuid: Uuid,
    pub backend: String,
}

/// Shared state passed to every Dropshot handler.
pub struct AgentContext {
    pub metadata: AgentMetadata,
    pub registry: TaskRegistry,
    /// Ring buffer of recent tasks. Held under a `std::sync::Mutex` because
    /// handlers only touch it briefly (push / truncate / clone to serialize).
    pub history: Mutex<VecDeque<TaskHistoryEntry>>,
    /// Set by `/pause`, cleared by `/resume`. When true, `/tasks` returns 503.
    pub paused: std::sync::atomic::AtomicBool,
}

impl AgentContext {
    pub fn new(metadata: AgentMetadata, registry: TaskRegistry) -> Self {
        Self {
            metadata,
            registry,
            history: Mutex::new(VecDeque::with_capacity(TASK_HISTORY_SIZE)),
            paused: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused
            .store(paused, std::sync::atomic::Ordering::Release);
    }

    /// Append an entry, evicting the oldest if we're at capacity.
    pub fn push_history(&self, entry: TaskHistoryEntry) {
        // Only ignore the poisoned-lock case: another thread panicked while
        // holding this mutex, but the state we're about to write is a fresh
        // entry — the prior contents can be salvaged. This is exactly the
        // pattern the Node.js agent relied on (best-effort logging).
        if let Ok(mut history) = self.history.lock() {
            if history.len() >= TASK_HISTORY_SIZE {
                history.pop_front();
            }
            history.push_back(entry);
        }
    }

    /// Clone the current history for serialization.
    ///
    /// Returns newest-first, matching the Node.js agent's display order.
    pub fn snapshot_history(&self) -> Vec<TaskHistoryEntry> {
        match self.history.lock() {
            Ok(history) => history.iter().rev().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }
}
