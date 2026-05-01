// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! First-run bootstrap for `tritond`.
//!
//! At every startup, [`ensure`] checks the store for the bits the
//! daemon can't run without:
//!
//! 1. A JWT signing key. Generated cryptographically and persisted
//!    under [`SystemKey::JwtSigning`] if absent.
//! 2. A root operator account. Created with a random base64 password
//!    if no users exist. The password is logged once to stderr with
//!    a clear "save this, it's only shown now" banner; it is not
//!    written anywhere else.
//!
//! Idempotent — subsequent runs find both records and do nothing.
//! The function returns the loaded [`JwtKey`] so the caller can hand
//! it to [`crate::auth::AuthService::new`].

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::info;
use tritond_auth::{JwtKey, generate_random_password, hash_password};
use tritond_store::{Store, StoreError, SystemKey, User};
use uuid::Uuid;

/// Username of the bootstrap root operator.
pub const ROOT_USERNAME: &str = "root";

/// Run first-run bootstrap if needed and return the JWT signing key.
pub async fn ensure(store: &dyn Store) -> Result<JwtKey> {
    let jwt_key = ensure_jwt_key(store).await?;
    ensure_root_user(store).await?;
    Ok(jwt_key)
}

async fn ensure_jwt_key(store: &dyn Store) -> Result<JwtKey> {
    match store.get_system_key(SystemKey::JwtSigning).await {
        Ok(bytes) => {
            let array: [u8; tritond_auth::jwt::JWT_KEY_BYTES] = bytes
                .as_slice()
                .try_into()
                .context("stored JWT signing key has wrong length")?;
            Ok(JwtKey::from_bytes(array))
        }
        Err(StoreError::NotFound) => {
            let key = JwtKey::generate();
            store
                .put_system_key(SystemKey::JwtSigning, key.bytes().to_vec())
                .await
                .context("persist JWT signing key")?;
            info!("generated and persisted new JWT signing key");
            Ok(key)
        }
        Err(e) => Err(anyhow::anyhow!("read JWT signing key: {e}")),
    }
}

async fn ensure_root_user(store: &dyn Store) -> Result<()> {
    if store
        .has_any_user()
        .await
        .context("check for existing users")?
    {
        return Ok(());
    }

    let plaintext = generate_random_password();
    let password_hash = hash_password(&plaintext)
        .await
        .context("hash bootstrap password")?;
    let user = User {
        id: Uuid::new_v4(),
        username: ROOT_USERNAME.to_string(),
        password_hash,
        is_root: true,
        created_at: Utc::now(),
        silo_id: None,
        federation: None,
    };
    store
        .create_user(user)
        .await
        .context("persist bootstrap root user")?;

    // Banner deliberately uses eprintln rather than `info!` so the
    // operator can see it even when tracing is filtered to warn-only.
    // `plaintext` is a `RedactedString`; we explicitly expose it once
    // here, then it is zeroed when the function returns.
    eprintln!();
    eprintln!("============================================================");
    eprintln!("  tritond bootstrap: created root operator");
    eprintln!();
    eprintln!("  username: {ROOT_USERNAME}");
    eprintln!("  password: {}", plaintext.expose());
    eprintln!();
    eprintln!("  Save this password now. It will not be shown again.");
    eprintln!("  Use `tcadm configure` to authenticate, then create");
    eprintln!("  long-lived API keys with `tcadm api-key create`.");
    eprintln!("============================================================");
    eprintln!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tritond_store::MemStore;

    #[tokio::test]
    async fn fresh_store_creates_root_and_persists_key() {
        let store = MemStore::new();
        let key1 = ensure(&store).await.unwrap();
        assert!(store.has_any_user().await.unwrap());
        let user = store.get_user_by_username(ROOT_USERNAME).await.unwrap();
        assert!(user.is_root);

        // Second call must be idempotent: same key, no second user.
        let key2 = ensure(&store).await.unwrap();
        assert_eq!(key1.bytes(), key2.bytes());
        assert_eq!(
            store.get_user_by_username(ROOT_USERNAME).await.unwrap().id,
            user.id
        );
    }
}
