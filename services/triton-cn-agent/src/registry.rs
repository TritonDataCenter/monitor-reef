// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Task handler registry.
//!
//! Each [`cn_agent_api::TaskName`] maps to a single [`TaskHandler`] trait
//! object. The service instantiates its registry at startup based on the
//! backend (SmartOS / dummy) and then `dispatch_task` looks up the handler by
//! task name.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskName, TaskResult};

/// A single task implementation.
///
/// Handlers receive the raw `params` object from the request body; they
/// typically deserialize it into a task-specific struct and then do the work.
#[async_trait]
pub trait TaskHandler: Send + Sync + 'static {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError>;
}

/// Registry of task handlers indexed by task name.
///
/// Cheap to clone; the inner map is `Arc`-wrapped.
#[derive(Clone, Default)]
pub struct TaskRegistry {
    handlers: Arc<HashMap<TaskName, Arc<dyn TaskHandler>>>,
}

impl TaskRegistry {
    pub fn builder() -> TaskRegistryBuilder {
        TaskRegistryBuilder::default()
    }

    pub fn get(&self, task: TaskName) -> Option<Arc<dyn TaskHandler>> {
        self.handlers.get(&task).cloned()
    }

    pub fn registered_tasks(&self) -> Vec<TaskName> {
        self.handlers.keys().copied().collect()
    }
}

/// Builder for [`TaskRegistry`].
#[derive(Default)]
pub struct TaskRegistryBuilder {
    handlers: HashMap<TaskName, Arc<dyn TaskHandler>>,
}

impl TaskRegistryBuilder {
    pub fn register<H>(mut self, task: TaskName, handler: H) -> Self
    where
        H: TaskHandler,
    {
        self.handlers.insert(task, Arc::new(handler));
        self
    }

    pub fn build(self) -> TaskRegistry {
        TaskRegistry {
            handlers: Arc::new(self.handlers),
        }
    }
}
