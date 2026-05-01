// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-memory [`Store`] backed by an `RwLock<HashMap>`.
//!
//! Used for unit tests, integration tests, and `tritond` runs that
//! don't need durable state.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{NewSilo, Silo, Store, StoreError};

#[derive(Default)]
struct Inner {
    silos_by_id: HashMap<Uuid, Silo>,
    silo_id_by_name: HashMap<String, Uuid>,
}

/// In-process [`Store`] implementation.
///
/// State is held behind a `tokio::sync::RwLock`; this is fine for
/// tests and small embedded uses but does not survive process
/// restarts.
pub struct MemStore {
    inner: RwLock<Inner>,
}

impl MemStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Store for MemStore {
    async fn create_silo(&self, req: NewSilo) -> Result<Silo, StoreError> {
        let mut guard = self.inner.write().await;

        if guard.silo_id_by_name.contains_key(&req.name) {
            return Err(StoreError::Conflict(format!(
                "silo with name {:?} already exists",
                req.name
            )));
        }

        let silo = Silo {
            id: Uuid::new_v4(),
            name: req.name.clone(),
            description: req.description.unwrap_or_default(),
            created_at: Utc::now(),
        };
        guard.silo_id_by_name.insert(silo.name.clone(), silo.id);
        guard.silos_by_id.insert(silo.id, silo.clone());
        Ok(silo)
    }

    async fn get_silo(&self, id: Uuid) -> Result<Silo, StoreError> {
        let guard = self.inner.read().await;
        guard
            .silos_by_id
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_then_get_returns_same_silo() {
        let store = MemStore::new();
        let created = store
            .create_silo(NewSilo {
                name: "operator".to_string(),
                description: Some("the bootstrap silo".to_string()),
            })
            .await
            .unwrap();
        let fetched = store.get_silo(created.id).await.unwrap();
        assert_eq!(created, fetched);
        assert_eq!(fetched.description, "the bootstrap silo");
    }

    #[tokio::test]
    async fn duplicate_name_conflicts() {
        let store = MemStore::new();
        store
            .create_silo(NewSilo {
                name: "ops".to_string(),
                description: None,
            })
            .await
            .unwrap();
        let err = store
            .create_silo(NewSilo {
                name: "ops".to_string(),
                description: None,
            })
            .await
            .expect_err("second create should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn get_unknown_id_is_not_found() {
        let store = MemStore::new();
        let err = store
            .get_silo(Uuid::new_v4())
            .await
            .expect_err("unknown id should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn missing_description_stored_as_empty_string() {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "blank".to_string(),
                description: None,
            })
            .await
            .unwrap();
        assert_eq!(silo.description, "");
    }
}
