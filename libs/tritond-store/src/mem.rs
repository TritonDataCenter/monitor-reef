// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-memory [`Store`] backed by `tokio::sync::RwLock<HashMap>`.
//!
//! Used for unit tests, integration tests, and `tritond` runs that
//! don't need durable state.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{ApiKey, NewSilo, Silo, Store, StoreError, SystemKey, User};

#[derive(Default)]
struct Inner {
    silos_by_id: HashMap<Uuid, Silo>,
    silo_id_by_name: HashMap<String, Uuid>,
    users_by_id: HashMap<Uuid, User>,
    user_id_by_username: HashMap<String, Uuid>,
    api_keys_by_id: HashMap<Uuid, ApiKey>,
    system_keys: HashMap<SystemKey, Vec<u8>>,
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

    async fn create_user(&self, user: User) -> Result<User, StoreError> {
        let mut guard = self.inner.write().await;
        if guard.user_id_by_username.contains_key(&user.username) {
            return Err(StoreError::Conflict(format!(
                "user with username {:?} already exists",
                user.username
            )));
        }
        guard
            .user_id_by_username
            .insert(user.username.clone(), user.id);
        guard.users_by_id.insert(user.id, user.clone());
        Ok(user)
    }

    async fn get_user_by_username(&self, username: &str) -> Result<User, StoreError> {
        let guard = self.inner.read().await;
        let id = guard
            .user_id_by_username
            .get(username)
            .copied()
            .ok_or(StoreError::NotFound)?;
        guard
            .users_by_id
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_user_by_id(&self, id: Uuid) -> Result<User, StoreError> {
        let guard = self.inner.read().await;
        guard
            .users_by_id
            .get(&id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn has_any_user(&self) -> Result<bool, StoreError> {
        let guard = self.inner.read().await;
        Ok(!guard.users_by_id.is_empty())
    }

    async fn create_api_key(&self, key: ApiKey) -> Result<ApiKey, StoreError> {
        let mut guard = self.inner.write().await;
        guard.api_keys_by_id.insert(key.id, key.clone());
        Ok(key)
    }

    async fn list_api_keys(&self, user_id: Uuid) -> Result<Vec<ApiKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard
            .api_keys_by_id
            .values()
            .filter(|k| k.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn all_api_keys(&self) -> Result<Vec<ApiKey>, StoreError> {
        let guard = self.inner.read().await;
        Ok(guard.api_keys_by_id.values().cloned().collect())
    }

    async fn delete_api_key(&self, user_id: Uuid, key_id: Uuid) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        match guard.api_keys_by_id.get(&key_id) {
            Some(k) if k.user_id == user_id => {
                guard.api_keys_by_id.remove(&key_id);
                Ok(())
            }
            _ => Err(StoreError::NotFound),
        }
    }

    async fn get_system_key(&self, key: SystemKey) -> Result<Vec<u8>, StoreError> {
        let guard = self.inner.read().await;
        guard
            .system_keys
            .get(&key)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn put_system_key(&self, key: SystemKey, value: Vec<u8>) -> Result<(), StoreError> {
        let mut guard = self.inner.write().await;
        guard.system_keys.insert(key, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_fixture(name: &str) -> User {
        User {
            id: Uuid::new_v4(),
            username: name.to_string(),
            password_hash: "$2y$12$dummyhash".to_string(),
            is_root: false,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn create_then_get_silo_returns_same_record() {
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
    }

    #[tokio::test]
    async fn duplicate_silo_name_conflicts() {
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
    async fn get_unknown_silo_id_is_not_found() {
        let store = MemStore::new();
        let err = store
            .get_silo(Uuid::new_v4())
            .await
            .expect_err("unknown id should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn missing_silo_description_stored_as_empty_string() {
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

    #[tokio::test]
    async fn user_round_trips_by_username_and_id() {
        let store = MemStore::new();
        let user = user_fixture("root");
        let user_id = user.id;
        let created = store.create_user(user).await.unwrap();
        assert_eq!(created.id, user_id);

        let by_username = store.get_user_by_username("root").await.unwrap();
        assert_eq!(by_username.id, user_id);

        let by_id = store.get_user_by_id(user_id).await.unwrap();
        assert_eq!(by_id.username, "root");
    }

    #[tokio::test]
    async fn duplicate_username_conflicts() {
        let store = MemStore::new();
        store.create_user(user_fixture("root")).await.unwrap();
        let err = store
            .create_user(user_fixture("root"))
            .await
            .expect_err("second create should conflict");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn has_any_user_flips_after_first_create() {
        let store = MemStore::new();
        assert!(!store.has_any_user().await.unwrap());
        store.create_user(user_fixture("root")).await.unwrap();
        assert!(store.has_any_user().await.unwrap());
    }

    #[tokio::test]
    async fn api_key_list_filters_by_owner_and_delete_removes() {
        let store = MemStore::new();
        let owner = user_fixture("alice");
        let other = user_fixture("bob");
        store.create_user(owner.clone()).await.unwrap();
        store.create_user(other.clone()).await.unwrap();

        let key_a = ApiKey {
            id: Uuid::new_v4(),
            user_id: owner.id,
            description: "ci".to_string(),
            hash: "$hashA".to_string(),
            created_at: Utc::now(),
        };
        let key_b = ApiKey {
            id: Uuid::new_v4(),
            user_id: other.id,
            description: "tf".to_string(),
            hash: "$hashB".to_string(),
            created_at: Utc::now(),
        };
        store.create_api_key(key_a.clone()).await.unwrap();
        store.create_api_key(key_b.clone()).await.unwrap();

        let owner_keys = store.list_api_keys(owner.id).await.unwrap();
        assert_eq!(owner_keys.len(), 1);
        assert_eq!(owner_keys[0].id, key_a.id);

        // delete by wrong owner is not-found
        let err = store
            .delete_api_key(other.id, key_a.id)
            .await
            .expect_err("cross-owner delete should be not-found");
        assert!(matches!(err, StoreError::NotFound));

        // delete by right owner works, then double-delete is not-found
        store.delete_api_key(owner.id, key_a.id).await.unwrap();
        let err = store
            .delete_api_key(owner.id, key_a.id)
            .await
            .expect_err("repeat delete should be not-found");
        assert!(matches!(err, StoreError::NotFound));
    }

    #[tokio::test]
    async fn system_key_round_trip() {
        let store = MemStore::new();
        let err = store
            .get_system_key(SystemKey::JwtSigning)
            .await
            .expect_err("missing key should be not-found");
        assert!(matches!(err, StoreError::NotFound));

        let payload = vec![0xAA; 32];
        store
            .put_system_key(SystemKey::JwtSigning, payload.clone())
            .await
            .unwrap();
        let read = store.get_system_key(SystemKey::JwtSigning).await.unwrap();
        assert_eq!(read, payload);
    }
}
