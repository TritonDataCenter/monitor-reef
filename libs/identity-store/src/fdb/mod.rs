// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FoundationDB-backed [`IdentityStore`] implementation.
//!
//! Compiled in only when the `foundationdb` cargo feature is enabled,
//! because linking pulls in `libfdb_c.so` (FoundationDB client library).
//! Default builds don't need FDB installed and use [`crate::MemStore`]
//! instead.
//!
//! # Boot semantics
//!
//! The FDB Rust binding requires exactly one `boot()` call per process;
//! the returned guard must outlive every `Database` handle. We satisfy
//! this with a `OnceLock` plus a `mem::forget` so the network thread runs
//! until the process exits — the right shape for a long-running daemon.
//! Mirrors `tritond-store`'s `fdb::ensure_fdb_booted`.
//!
//! # Schema
//!
//! Every key shape lives in [`keys`], under the `identity/…` root (disjoint
//! from tritond's `triton/…` keyspace). Writes that touch multiple keys ride
//! a single transaction so name uniqueness and index consistency are
//! enforced atomically: a duplicate username, a second `System` realm, or a
//! cross-silo grant are rejected transactionally, first-writer-wins.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use foundationdb::future::FdbValues;
use foundationdb::options::StreamingMode;
use foundationdb::{Database, FdbBindingError, KeySelector, RangeOption, Transaction};
use uuid::Uuid;

use crate::types::*;
use crate::{IdentityStore, StoreError};

mod keys;

static FDB_NETWORK: OnceLock<()> = OnceLock::new();

/// Boot the FDB network thread (idempotent). The returned guard is
/// intentionally leaked so FDB stays alive for the rest of the process.
fn ensure_fdb_booted() {
    FDB_NETWORK.get_or_init(|| {
        // SAFETY: boot() must be called at most once per process. The
        // OnceLock guarantees that. The returned guard is leaked so it
        // outlives all Database instances, which is the requirement.
        let guard = unsafe { foundationdb::boot() };
        std::mem::forget(guard);
    });
}

impl From<FdbBindingError> for StoreError {
    fn from(e: FdbBindingError) -> Self {
        StoreError::Backend(format!("FDB transaction: {e}"))
    }
}

/// Outside-closure deserialize error mapper.
fn de_err(name: &'static str) -> impl FnOnce(serde_json::Error) -> StoreError {
    move |e| StoreError::Backend(format!("deserialize {name}: {e}"))
}

/// Inside-closure serialize error mapper.
fn txn_ser_err(name: &'static str) -> impl FnOnce(serde_json::Error) -> FdbBindingError {
    move |e| FdbBindingError::CustomError(format!("serialize {name}: {e}").into())
}

/// Inside-closure deserialize error mapper.
fn txn_de_err(name: &'static str) -> impl FnOnce(serde_json::Error) -> FdbBindingError {
    move |e| FdbBindingError::CustomError(format!("deserialize {name}: {e}").into())
}

/// Inside-closure general error mapper (UTF-8 conversions, UUID parsing).
fn txn_err<E: std::fmt::Display>(ctx: &'static str) -> impl FnOnce(E) -> FdbBindingError {
    move |e| FdbBindingError::CustomError(format!("{ctx}: {e}").into())
}

/// Serialize `value` to JSON and write it at `key` inside a txn closure.
fn txn_set<T: serde::Serialize>(
    tr: &Transaction,
    key: &[u8],
    value: &T,
    kind: &'static str,
) -> Result<(), FdbBindingError> {
    let bytes = serde_json::to_vec(value).map_err(txn_ser_err(kind))?;
    tr.set(key, &bytes);
    Ok(())
}

/// Run an FDB transaction. Clones each named capture once per retry
/// iteration (the binding's `db.run` calls the closure `FnMut`, so captures
/// must be owned). Body sees `tr: &Transaction` and returns
/// `Result<T, FdbBindingError>`. Mirrors `tritond-store`'s `fdb_txn!`.
macro_rules! fdb_txn {
    ($db:expr, [$($capture:ident),* $(,)?], |$tr:ident| $body:block) => {{
        $db.run(|$tr, _| {
            $(let $capture = $capture.clone();)*
            async move $body
        }).await
    }};
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

fn now() -> DateTime<Utc> {
    Utc::now()
}

/// A `RangeOption` over the half-open prefix range `[prefix, prefix++)`,
/// configured to drain the whole range. `StreamingMode::WantAll` asks the
/// server for as many rows per batch as it can, and [`drain_range`] /
/// [`drain_range_values`] still loop on [`FdbValues::more`] so a range
/// larger than a single batch is read to completion rather than silently
/// truncated.
fn full_scan_opt(prefix: &[u8]) -> RangeOption<'static> {
    let (begin, end) = prefix_range(prefix);
    RangeOption {
        begin: KeySelector::first_greater_or_equal(begin),
        end: KeySelector::first_greater_or_equal(end),
        mode: StreamingMode::WantAll,
        ..RangeOption::default()
    }
}

/// Drain a prefix range to completion inside a txn, yielding every value's
/// owned bytes. Loops across FDB batches (`more()`), so a range that
/// exceeds one batch is fully read instead of truncated at the first.
async fn drain_range_values(
    tr: &Transaction,
    prefix: &[u8],
) -> Result<Vec<Vec<u8>>, FdbBindingError> {
    let mut out = Vec::new();
    drain_range(tr, prefix, |kv| out.push(kv.value().to_vec())).await?;
    Ok(out)
}

/// Drain a prefix range to completion inside a txn, calling `visit` once
/// per key/value pair across every batch. The visitor borrows the
/// [`FdbValues`] entry; copy out whatever you need to keep.
async fn drain_range<F>(
    tr: &Transaction,
    prefix: &[u8],
    mut visit: F,
) -> Result<(), FdbBindingError>
where
    F: FnMut(&foundationdb::future::FdbKeyValue),
{
    let mut opt = Some(full_scan_opt(prefix));
    let mut iteration = 1usize;
    while let Some(current) = opt.take() {
        let kvs: FdbValues = tr.get_range(&current, iteration, false).await?;
        for kv in kvs.iter() {
            visit(kv);
        }
        // `next_range` returns `None` once `more()` is false, terminating
        // the loop; otherwise it advances `begin` past the last key read.
        opt = current.next_range(&kvs);
        iteration += 1;
    }
    Ok(())
}

/// FoundationDB-backed [`IdentityStore`].
pub struct FdbStore {
    db: Arc<Database>,
}

impl FdbStore {
    /// Open the database described by `cluster_file_path`. Pass `None` to
    /// use FoundationDB's default cluster file resolution
    /// (`FDB_CLUSTER_FILE` env, `/etc/foundationdb/fdb.cluster`).
    pub fn open(cluster_file_path: Option<&str>) -> Result<Self, StoreError> {
        ensure_fdb_booted();
        let db = Database::new(cluster_file_path)
            .map_err(|e| StoreError::Backend(format!("open FDB cluster: {e}")))?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Get a shared handle to the underlying [`foundationdb::Database`].
    pub fn database(&self) -> Arc<Database> {
        Arc::clone(&self.db)
    }

    /// Read the value for a single key, returning `None` if absent.
    async fn read_bytes(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let key = key.to_vec();
        let result: Result<Option<Vec<u8>>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move { Ok(tr.get(&key, false).await?.map(|s| s.to_vec())) }
            })
            .await;
        result.map_err(StoreError::from)
    }

    /// Read + JSON-decode a single record by key, mapping absence to
    /// [`StoreError::NotFound`].
    async fn read_record<T: serde::de::DeserializeOwned>(
        &self,
        key: &[u8],
        kind: &'static str,
    ) -> Result<T, StoreError> {
        let bytes = self.read_bytes(key).await?.ok_or(StoreError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(de_err(kind))
    }

    /// Follow an index whose value is a uuid string, returning the parsed
    /// uuid (or `None` if the index key is absent).
    async fn read_index_uuid(&self, key: &[u8]) -> Result<Option<Uuid>, StoreError> {
        let Some(bytes) = self.read_bytes(key).await? else {
            return Ok(None);
        };
        let s = std::str::from_utf8(&bytes)
            .map_err(|e| StoreError::Backend(format!("index value not utf8: {e}")))?;
        let id = Uuid::parse_str(s)
            .map_err(|e| StoreError::Backend(format!("index value not uuid: {e}")))?;
        Ok(Some(id))
    }

    /// Range-scan a prefix and return every value's raw bytes. Drains the
    /// whole range across batches (see [`drain_range_values`]).
    async fn scan_values(&self, prefix: Vec<u8>) -> Result<Vec<Vec<u8>>, StoreError> {
        let values: Result<Vec<Vec<u8>>, FdbBindingError> = fdb_txn!(self.db, [prefix], |tr| {
            drain_range_values(&tr, &prefix).await
        });
        values.map_err(StoreError::from)
    }

    /// Range-scan a membership-index prefix and return the trailing uuid of
    /// every key (the segment after `prefix`). Drains the whole range across
    /// batches via [`scan_suffix_uuids_in_txn`].
    async fn scan_suffix_uuids(&self, prefix: Vec<u8>) -> Result<Vec<Uuid>, StoreError> {
        let ids: Result<Vec<Uuid>, FdbBindingError> = fdb_txn!(self.db, [prefix], |tr| {
            scan_suffix_uuids_in_txn(&tr, &prefix).await
        });
        ids.map_err(StoreError::from)
    }

    /// Fetch every record in a `prefix` whose value is a uuid pointing at a
    /// `by_id` record, deserializing each. Used by the per-realm lists.
    async fn list_by_membership<T, F>(
        &self,
        prefix: Vec<u8>,
        by_id_key: F,
        kind: &'static str,
    ) -> Result<Vec<T>, StoreError>
    where
        T: serde::de::DeserializeOwned,
        F: Fn(Uuid) -> Vec<u8>,
    {
        let ids = self.scan_suffix_uuids(prefix).await?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(bytes) = self.read_bytes(&by_id_key(id)).await? {
                out.push(serde_json::from_slice(&bytes).map_err(de_err(kind))?);
            }
        }
        Ok(out)
    }
}

/// Generic "create a record + indices in one transaction" outcome.
enum CreateOutcome {
    Created,
    Conflict(String),
    NotFound,
}

#[async_trait]
impl IdentityStore for FdbStore {
    // ─────────────────────────────── Realms ───────────────────────────────

    async fn create_realm(
        &self,
        req: NewRealm,
        initial_keys: Vec<NewSigningKey>,
    ) -> Result<Realm, StoreError> {
        if initial_keys.is_empty() {
            return Err(StoreError::Conflict(
                "a realm must be created with at least one signing key".into(),
            ));
        }
        // kid uniqueness within the seed ring (matches MemStore).
        {
            let mut seen = std::collections::HashSet::new();
            for k in &initial_keys {
                if !seen.insert(k.kid.clone()) {
                    return Err(StoreError::Conflict(format!(
                        "duplicate kid {:?} in initial key ring",
                        k.kid
                    )));
                }
            }
        }

        let id = Uuid::new_v4();
        let created_at = now();
        let realm = Realm {
            id,
            scope: req.scope,
            name: req.name,
            description: req.description.unwrap_or_default(),
            issuer_url: req.issuer_url,
            signing_alg: req.signing_alg.unwrap_or_default(),
            access_token_ttl_secs: req
                .access_token_ttl_secs
                .unwrap_or(DEFAULT_ACCESS_TOKEN_TTL_SECS),
            id_token_ttl_secs: req.id_token_ttl_secs.unwrap_or(DEFAULT_ID_TOKEN_TTL_SECS),
            refresh_token_ttl_secs: req
                .refresh_token_ttl_secs
                .unwrap_or(DEFAULT_REFRESH_TOKEN_TTL_SECS),
            auth_code_ttl_secs: req.auth_code_ttl_secs.unwrap_or(DEFAULT_AUTH_CODE_TTL_SECS),
            device_code_ttl_secs: req
                .device_code_ttl_secs
                .unwrap_or(DEFAULT_DEVICE_CODE_TTL_SECS),
            login_policy: req.login_policy.unwrap_or_default(),
            created_at,
        };
        let keys_seed: Vec<SigningKey> = initial_keys
            .into_iter()
            .map(|k| SigningKey {
                kid: k.kid,
                realm_id: id,
                alg: k.alg,
                private_pem: k.private_pem,
                public_jwk: k.public_jwk,
                status: k.status,
                not_before: k.not_before,
                not_after: k.not_after,
                created_at,
            })
            .collect();

        let by_id = keys::realm_by_id_key(id);
        let by_issuer = keys::realm_by_issuer_key(&realm.issuer_url);
        let by_scope = keys::realm_by_scope_key(&realm.scope);
        let all = keys::realm_all_key(id);
        let id_str = id.to_string();
        let scope_tag = realm.scope.tag().to_string();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let realm = realm.clone();
                let keys_seed = keys_seed.clone();
                let by_id = by_id.clone();
                let by_issuer = by_issuer.clone();
                let by_scope = by_scope.clone();
                let all = all.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                let scope_tag = scope_tag.clone();
                let issuer = realm.issuer_url.clone();
                async move {
                    if tr.get(&by_issuer, false).await?.is_some() {
                        return Ok(CreateOutcome::Conflict(format!(
                            "issuer {issuer:?} is already claimed by another realm"
                        )));
                    }
                    if tr.get(&by_scope, false).await?.is_some() {
                        return Ok(CreateOutcome::Conflict(format!(
                            "a realm with scope {scope_tag} already exists"
                        )));
                    }
                    txn_set(&tr, &by_id, &realm, "realm")?;
                    tr.set(&by_issuer, &id_bytes);
                    tr.set(&by_scope, &id_bytes);
                    tr.set(&all, b"");
                    for k in &keys_seed {
                        let key = keys::signing_key_key(realm.id, &k.kid);
                        txn_set(&tr, &key, k, "signing key")?;
                    }
                    Ok(CreateOutcome::Created)
                }
            })
            .await;

        match outcome {
            Ok(CreateOutcome::Created) => Ok(realm),
            Ok(CreateOutcome::Conflict(m)) => Err(StoreError::Conflict(m)),
            Ok(CreateOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_realm(&self, id: Uuid) -> Result<Realm, StoreError> {
        self.read_record(&keys::realm_by_id_key(id), "realm").await
    }

    async fn get_realm_by_issuer(&self, issuer: &str) -> Result<Realm, StoreError> {
        let id = self
            .read_index_uuid(&keys::realm_by_issuer_key(issuer))
            .await?
            .ok_or(StoreError::NotFound)?;
        self.get_realm(id).await
    }

    async fn get_realm_by_scope(&self, scope: &RealmScope) -> Result<Realm, StoreError> {
        let id = self
            .read_index_uuid(&keys::realm_by_scope_key(scope))
            .await?
            .ok_or(StoreError::NotFound)?;
        self.get_realm(id).await
    }

    async fn list_realms(&self) -> Result<Vec<Realm>, StoreError> {
        self.list_by_membership(keys::realm_all_prefix(), keys::realm_by_id_key, "realm")
            .await
    }

    async fn update_realm_settings(
        &self,
        id: Uuid,
        settings: RealmSettings,
    ) -> Result<Realm, StoreError> {
        let by_id = keys::realm_by_id_key(id);
        let result: Result<Option<Realm>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let settings = settings.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(None);
                    };
                    let mut realm: Realm =
                        serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("realm"))?;
                    realm.access_token_ttl_secs = settings.access_token_ttl_secs;
                    realm.id_token_ttl_secs = settings.id_token_ttl_secs;
                    realm.refresh_token_ttl_secs = settings.refresh_token_ttl_secs;
                    realm.auth_code_ttl_secs = settings.auth_code_ttl_secs;
                    realm.device_code_ttl_secs = settings.device_code_ttl_secs;
                    realm.login_policy = settings.login_policy;
                    txn_set(&tr, &by_id, &realm, "realm")?;
                    Ok(Some(realm))
                }
            })
            .await;
        match result {
            Ok(Some(r)) => Ok(r),
            Ok(None) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete_realm(&self, id: Uuid) -> Result<(), StoreError> {
        let by_id = keys::realm_by_id_key(id);
        let users_pfx = keys::user_in_realm_prefix(id);
        let clients_pfx = keys::client_in_realm_prefix(id);
        let conns_pfx = keys::conn_in_realm_prefix(id);
        let signkey_pfx = keys::signing_key_prefix(id);

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let users_pfx = users_pfx.clone();
                let clients_pfx = clients_pfx.clone();
                let conns_pfx = conns_pfx.clone();
                let signkey_pfx = signkey_pfx.clone();
                async move {
                    let Some(realm_bytes) = tr.get(&by_id, false).await? else {
                        return Ok(CreateOutcome::NotFound);
                    };
                    let realm: Realm =
                        serde_json::from_slice(realm_bytes.as_ref()).map_err(txn_de_err("realm"))?;
                    // No cascade: refuse if any child still references it.
                    for pfx in [&users_pfx, &clients_pfx, &conns_pfx] {
                        if !range_is_empty(&tr, pfx).await? {
                            return Ok(CreateOutcome::Conflict(
                                "realm still has users, clients, or connections".into(),
                            ));
                        }
                    }
                    tr.clear(&by_id);
                    tr.clear(&keys::realm_by_issuer_key(&realm.issuer_url));
                    tr.clear(&keys::realm_by_scope_key(&realm.scope));
                    tr.clear(&keys::realm_all_key(id));
                    // The ring is owned by the realm; drop it with the realm.
                    let (b, e) = prefix_range(&signkey_pfx);
                    tr.clear_range(&b, &e);
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    // ─────────────────────────────── Users ────────────────────────────────

    async fn create_user(&self, req: NewUser) -> Result<User, StoreError> {
        let user = User {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            display_name: req.display_name.unwrap_or_else(|| req.username.clone()),
            username: req.username,
            email: req.email,
            password_hash: req.password_hash,
            is_root: req.is_root,
            fleet_admin: req.fleet_admin,
            status: UserStatus::Active,
            mfa: None,
            brokered: req.brokered,
            created_at: now(),
        };

        let realm_by_id = keys::realm_by_id_key(user.realm_id);
        let by_id = keys::user_by_id_key(user.id);
        let by_username = keys::user_by_username_key(user.realm_id, &user.username);
        let by_email = user
            .email
            .as_ref()
            .map(|e| keys::user_by_email_key(user.realm_id, e));
        let by_brokered = user
            .brokered
            .as_ref()
            .map(|b| keys::user_by_brokered_key(user.realm_id, b.connection_id, &b.upstream_subject));
        let in_realm = keys::user_in_realm_key(user.realm_id, user.id);
        let id_str = user.id.to_string();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let user = user.clone();
                let realm_by_id = realm_by_id.clone();
                let by_id = by_id.clone();
                let by_username = by_username.clone();
                let by_email = by_email.clone();
                let by_brokered = by_brokered.clone();
                let in_realm = in_realm.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&realm_by_id, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    if tr.get(&by_username, false).await?.is_some() {
                        return Ok(CreateOutcome::Conflict(format!(
                            "username {:?} already exists in this realm",
                            user.username
                        )));
                    }
                    if let Some(ek) = by_email.as_deref()
                        && tr.get(ek, false).await?.is_some()
                    {
                        return Ok(CreateOutcome::Conflict(format!(
                            "email {:?} already exists in this realm",
                            user.email
                        )));
                    }
                    txn_set(&tr, &by_id, &user, "user")?;
                    tr.set(&by_username, &id_bytes);
                    if let Some(ek) = by_email.as_deref() {
                        tr.set(ek, &id_bytes);
                    }
                    if let Some(bk) = by_brokered.as_deref() {
                        tr.set(bk, &id_bytes);
                    }
                    tr.set(&in_realm, b"");
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        match outcome {
            Ok(CreateOutcome::Created) => Ok(user),
            Ok(CreateOutcome::Conflict(m)) => Err(StoreError::Conflict(m)),
            Ok(CreateOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_user(&self, id: Uuid) -> Result<User, StoreError> {
        self.read_record(&keys::user_by_id_key(id), "user").await
    }

    async fn get_user_by_username(
        &self,
        realm_id: Uuid,
        username: &str,
    ) -> Result<User, StoreError> {
        let id = self
            .read_index_uuid(&keys::user_by_username_key(realm_id, username))
            .await?
            .ok_or(StoreError::NotFound)?;
        self.get_user(id).await
    }

    async fn get_user_by_brokered(
        &self,
        realm_id: Uuid,
        connection_id: Uuid,
        upstream_subject: &str,
    ) -> Result<User, StoreError> {
        let id = self
            .read_index_uuid(&keys::user_by_brokered_key(
                realm_id,
                connection_id,
                upstream_subject,
            ))
            .await?
            .ok_or(StoreError::NotFound)?;
        self.get_user(id).await
    }

    async fn list_users_in_realm(&self, realm_id: Uuid) -> Result<Vec<User>, StoreError> {
        self.list_by_membership(
            keys::user_in_realm_prefix(realm_id),
            keys::user_by_id_key,
            "user",
        )
        .await
    }

    async fn update_user_password_hash(
        &self,
        id: Uuid,
        password_hash: String,
    ) -> Result<User, StoreError> {
        let by_id = keys::user_by_id_key(id);
        let result: Result<Option<User>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let password_hash = password_hash.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(None);
                    };
                    let mut user: User =
                        serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("user"))?;
                    user.password_hash = password_hash;
                    txn_set(&tr, &by_id, &user, "user")?;
                    Ok(Some(user))
                }
            })
            .await;
        opt_to_result(result)
    }

    async fn set_user_status(&self, id: Uuid, status: UserStatus) -> Result<User, StoreError> {
        let by_id = keys::user_by_id_key(id);
        let result: Result<Option<User>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(None);
                    };
                    let mut user: User =
                        serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("user"))?;
                    user.status = status;
                    txn_set(&tr, &by_id, &user, "user")?;
                    if status == UserStatus::Disabled {
                        // Revoke every refresh token belonging to the user via
                        // the per-user index. Collect the jtis first (dropping
                        // the non-Send range iterator) before awaiting.
                        let pfx = keys::refresh_user_prefix(id);
                        for jti in scan_suffix_uuids_in_txn(&tr, &pfx).await? {
                            revoke_refresh_in_txn(&tr, jti).await?;
                        }
                    }
                    Ok(Some(user))
                }
            })
            .await;
        opt_to_result(result)
    }

    async fn delete_user(&self, id: Uuid) -> Result<(), StoreError> {
        // Pre-scan the indices that need range deletes; the membership rows
        // are read inside the txn so the delete is atomic.
        let by_id = keys::user_by_id_key(id);
        let user_group_pfx = keys::user_group_prefix(id);
        let refresh_user_pfx = keys::refresh_user_prefix(id);
        let session_user_pfx = keys::session_by_user_prefix(id);
        let role_subject = keys::role_by_subject_prefix(&AssignmentSubject::User { user_id: id });

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let user_group_pfx = user_group_pfx.clone();
                let refresh_user_pfx = refresh_user_pfx.clone();
                let session_user_pfx = session_user_pfx.clone();
                let role_subject = role_subject.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(CreateOutcome::NotFound);
                    };
                    let user: User =
                        serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("user"))?;
                    tr.clear(&by_id);
                    tr.clear(&keys::user_by_username_key(user.realm_id, &user.username));
                    if let Some(email) = &user.email {
                        tr.clear(&keys::user_by_email_key(user.realm_id, email));
                    }
                    if let Some(b) = &user.brokered {
                        tr.clear(&keys::user_by_brokered_key(
                            user.realm_id,
                            b.connection_id,
                            &b.upstream_subject,
                        ));
                    }
                    tr.clear(&keys::user_in_realm_key(user.realm_id, id));

                    // Group memberships: clear both edges via the reverse index.
                    for gid in scan_suffix_uuids_in_txn(&tr, &user_group_pfx).await? {
                        tr.clear(&keys::group_member_key(gid, id));
                        tr.clear(&keys::user_group_key(id, gid));
                    }
                    // Refresh tokens owned by the user (+ their family/by_id).
                    for jti in scan_suffix_uuids_in_txn(&tr, &refresh_user_pfx).await? {
                        clear_refresh_in_txn(&tr, jti).await?;
                    }
                    // Sessions owned by the user, via the per-user reverse
                    // index — clear both the record and its index edge.
                    for sid in scan_suffix_uuids_in_txn(&tr, &session_user_pfx).await? {
                        tr.clear(&keys::session_by_id_key(sid));
                        tr.clear(&keys::session_by_user_key(id, sid));
                    }
                    // User-typed role assignments (+ their indices).
                    for aid in scan_suffix_uuids_in_txn(&tr, &role_subject).await? {
                        clear_role_assignment_in_txn(&tr, aid).await?;
                    }
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn has_any_user_in_realm(&self, realm_id: Uuid) -> Result<bool, StoreError> {
        let prefix = keys::user_in_realm_prefix(realm_id);
        let (begin, end) = prefix_range(&prefix);
        let any: Result<bool, FdbBindingError> = fdb_txn!(self.db, [begin, end], |tr| {
            let opt = RangeOption {
                begin: KeySelector::first_greater_or_equal(begin),
                end: KeySelector::first_greater_or_equal(end),
                limit: Some(1),
                ..RangeOption::default()
            };
            let kvs = tr.get_range(&opt, 1, false).await?;
            Ok(!kvs.is_empty())
        });
        any.map_err(StoreError::from)
    }

    // ─────────────────────────────── Groups ───────────────────────────────

    async fn create_group(&self, req: NewGroup) -> Result<Group, StoreError> {
        let g = Group {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            name: req.name,
            description: req.description.unwrap_or_default(),
            created_at: now(),
        };
        let realm_by_id = keys::realm_by_id_key(g.realm_id);
        let by_id = keys::group_by_id_key(g.id);
        let by_name = keys::group_by_name_key(g.realm_id, &g.name);
        let in_realm = keys::group_in_realm_key(g.realm_id, g.id);
        let id_str = g.id.to_string();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let g = g.clone();
                let realm_by_id = realm_by_id.clone();
                let by_id = by_id.clone();
                let by_name = by_name.clone();
                let in_realm = in_realm.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&realm_by_id, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    if tr.get(&by_name, false).await?.is_some() {
                        return Ok(CreateOutcome::Conflict(format!(
                            "group {:?} already exists in this realm",
                            g.name
                        )));
                    }
                    txn_set(&tr, &by_id, &g, "group")?;
                    tr.set(&by_name, &id_bytes);
                    tr.set(&in_realm, b"");
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        match outcome {
            Ok(CreateOutcome::Created) => Ok(g),
            Ok(CreateOutcome::Conflict(m)) => Err(StoreError::Conflict(m)),
            Ok(CreateOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_group(&self, id: Uuid) -> Result<Group, StoreError> {
        self.read_record(&keys::group_by_id_key(id), "group").await
    }

    async fn list_groups_in_realm(&self, realm_id: Uuid) -> Result<Vec<Group>, StoreError> {
        self.list_by_membership(
            keys::group_in_realm_prefix(realm_id),
            keys::group_by_id_key,
            "group",
        )
        .await
    }

    async fn delete_group(&self, id: Uuid) -> Result<(), StoreError> {
        let by_id = keys::group_by_id_key(id);
        let member_pfx = keys::group_member_prefix(id);
        let role_subject = keys::role_by_subject_prefix(&AssignmentSubject::Group { group_id: id });

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let member_pfx = member_pfx.clone();
                let role_subject = role_subject.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(CreateOutcome::NotFound);
                    };
                    let g: Group =
                        serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("group"))?;
                    tr.clear(&by_id);
                    tr.clear(&keys::group_by_name_key(g.realm_id, &g.name));
                    tr.clear(&keys::group_in_realm_key(g.realm_id, id));
                    // Membership: clear both edges.
                    for uid in scan_suffix_uuids_in_txn(&tr, &member_pfx).await? {
                        tr.clear(&keys::group_member_key(id, uid));
                        tr.clear(&keys::user_group_key(uid, id));
                    }
                    // Group-typed role assignments cascade.
                    for aid in scan_suffix_uuids_in_txn(&tr, &role_subject).await? {
                        clear_role_assignment_in_txn(&tr, aid).await?;
                    }
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn add_group_member(&self, group_id: Uuid, user_id: Uuid) -> Result<(), StoreError> {
        let group_key = keys::group_by_id_key(group_id);
        let user_key = keys::user_by_id_key(user_id);
        let member = keys::group_member_key(group_id, user_id);
        let reverse = keys::user_group_key(user_id, group_id);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let group_key = group_key.clone();
                let user_key = user_key.clone();
                let member = member.clone();
                let reverse = reverse.clone();
                async move {
                    if tr.get(&group_key, false).await?.is_none()
                        || tr.get(&user_key, false).await?.is_none()
                    {
                        return Ok(CreateOutcome::NotFound);
                    }
                    tr.set(&member, b""); // idempotent
                    tr.set(&reverse, b"");
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn remove_group_member(&self, group_id: Uuid, user_id: Uuid) -> Result<(), StoreError> {
        let group_key = keys::group_by_id_key(group_id);
        let user_key = keys::user_by_id_key(user_id);
        let member = keys::group_member_key(group_id, user_id);
        let reverse = keys::user_group_key(user_id, group_id);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let group_key = group_key.clone();
                let user_key = user_key.clone();
                let member = member.clone();
                let reverse = reverse.clone();
                async move {
                    if tr.get(&group_key, false).await?.is_none()
                        || tr.get(&user_key, false).await?.is_none()
                    {
                        return Ok(CreateOutcome::NotFound);
                    }
                    tr.clear(&member); // no-op if absent
                    tr.clear(&reverse);
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn list_group_members(&self, group_id: Uuid) -> Result<Vec<Uuid>, StoreError> {
        if self
            .read_bytes(&keys::group_by_id_key(group_id))
            .await?
            .is_none()
        {
            return Err(StoreError::NotFound);
        }
        self.scan_suffix_uuids(keys::group_member_prefix(group_id))
            .await
    }

    async fn list_groups_of_user(&self, user_id: Uuid) -> Result<Vec<Uuid>, StoreError> {
        if self
            .read_bytes(&keys::user_by_id_key(user_id))
            .await?
            .is_none()
        {
            return Err(StoreError::NotFound);
        }
        self.scan_suffix_uuids(keys::user_group_prefix(user_id))
            .await
    }

    // ───────────────────────── Role assignments ───────────────────────────

    async fn create_role_assignment(
        &self,
        req: NewRoleAssignment,
    ) -> Result<RoleAssignment, StoreError> {
        let a = RoleAssignment {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            subject: req.subject,
            target: req.target,
            role: req.role,
            created_at: now(),
            created_by: req.created_by,
        };
        let realm_by_id = keys::realm_by_id_key(a.realm_id);
        let subject_exists_key = match &a.subject {
            AssignmentSubject::User { user_id } => keys::user_by_id_key(*user_id),
            AssignmentSubject::Group { group_id } => keys::group_by_id_key(*group_id),
        };
        let by_id = keys::role_by_id_key(a.id);
        let by_subject = keys::role_by_subject_key(&a.subject, a.id);
        let by_target = keys::role_by_target_key(&a.target, a.id);
        let dup = keys::role_dup_key(a.realm_id, &a.subject, &a.target, a.role);
        let id_str = a.id.to_string();
        let realm_id = a.realm_id;

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let a = a.clone();
                let realm_by_id = realm_by_id.clone();
                let subject_exists_key = subject_exists_key.clone();
                let by_id = by_id.clone();
                let by_subject = by_subject.clone();
                let by_target = by_target.clone();
                let dup = dup.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    // Realm must exist; load its scope for the structural check.
                    let Some(realm_bytes) = tr.get(&realm_by_id, false).await? else {
                        return Ok(CreateOutcome::NotFound);
                    };
                    let realm: Realm = serde_json::from_slice(realm_bytes.as_ref())
                        .map_err(txn_de_err("realm"))?;
                    if let Err(m) = check_grant_scope(&realm.scope, &a.target) {
                        return Ok(CreateOutcome::Conflict(m));
                    }
                    // Subject must exist in this realm.
                    match tr.get(&subject_exists_key, false).await? {
                        None => return Ok(CreateOutcome::NotFound),
                        Some(sb) => {
                            let in_realm = subject_in_realm(&a.subject, &sb, realm_id)
                                .map_err(txn_de_err("subject"))?;
                            if !in_realm {
                                return Ok(CreateOutcome::NotFound);
                            }
                        }
                    }
                    // Exact-tuple uniqueness.
                    if tr.get(&dup, false).await?.is_some() {
                        return Ok(CreateOutcome::Conflict(
                            "an identical role assignment already exists".into(),
                        ));
                    }
                    txn_set(&tr, &by_id, &a, "role assignment")?;
                    tr.set(&by_subject, b"");
                    tr.set(&by_target, b"");
                    tr.set(&dup, &id_bytes);
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        match outcome {
            Ok(CreateOutcome::Created) => Ok(a),
            Ok(CreateOutcome::Conflict(m)) => Err(StoreError::Conflict(m)),
            Ok(CreateOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_role_assignment(&self, id: Uuid) -> Result<RoleAssignment, StoreError> {
        self.read_record(&keys::role_by_id_key(id), "role assignment")
            .await
    }

    async fn list_assignments_of_subject(
        &self,
        subject: &AssignmentSubject,
    ) -> Result<Vec<RoleAssignment>, StoreError> {
        self.list_by_membership(
            keys::role_by_subject_prefix(subject),
            keys::role_by_id_key,
            "role assignment",
        )
        .await
    }

    async fn list_assignments_for_target(
        &self,
        target: &AssignmentTarget,
    ) -> Result<Vec<RoleAssignment>, StoreError> {
        self.list_by_membership(
            keys::role_by_target_prefix(target),
            keys::role_by_id_key,
            "role assignment",
        )
        .await
    }

    async fn delete_role_assignment(&self, id: Uuid) -> Result<(), StoreError> {
        let by_id = keys::role_by_id_key(id);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                async move {
                    if tr.get(&by_id, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    clear_role_assignment_in_txn(&tr, id).await?;
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    // ───────────────────────────── OAuth clients ──────────────────────────

    async fn create_oauth_client(&self, req: NewOAuthClient) -> Result<OAuthClient, StoreError> {
        let c = OAuthClient {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            name: req.name,
            client_secret_hash: req.client_secret_hash,
            redirect_uris: req.redirect_uris,
            grant_types: req.grant_types,
            pkce_required: req.pkce_required,
            scopes_allowed: req.scopes_allowed,
            is_workload: req.is_workload,
            bound_to_cn: req.bound_to_cn,
            created_at: now(),
        };
        let realm_by_id = keys::realm_by_id_key(c.realm_id);
        let by_id = keys::client_by_id_key(c.id);
        let by_cn = c.bound_to_cn.map(keys::client_by_cn_key);
        let in_realm = keys::client_in_realm_key(c.realm_id, c.id);
        let id_str = c.id.to_string();
        let bound_cn = c.bound_to_cn;

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let c = c.clone();
                let realm_by_id = realm_by_id.clone();
                let by_id = by_id.clone();
                let by_cn = by_cn.clone();
                let in_realm = in_realm.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&realm_by_id, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    if let Some(cnk) = by_cn.as_deref()
                        && tr.get(cnk, false).await?.is_some()
                    {
                        return Ok(CreateOutcome::Conflict(format!(
                            "compute node {} already has a bound client",
                            bound_cn.map(|x| x.to_string()).unwrap_or_default()
                        )));
                    }
                    txn_set(&tr, &by_id, &c, "oauth client")?;
                    if let Some(cnk) = by_cn.as_deref() {
                        tr.set(cnk, &id_bytes);
                    }
                    tr.set(&in_realm, b"");
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        match outcome {
            Ok(CreateOutcome::Created) => Ok(c),
            Ok(CreateOutcome::Conflict(m)) => Err(StoreError::Conflict(m)),
            Ok(CreateOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_oauth_client(&self, id: Uuid) -> Result<OAuthClient, StoreError> {
        self.read_record(&keys::client_by_id_key(id), "oauth client")
            .await
    }

    async fn get_oauth_client_by_cn(&self, server_uuid: Uuid) -> Result<OAuthClient, StoreError> {
        let id = self
            .read_index_uuid(&keys::client_by_cn_key(server_uuid))
            .await?
            .ok_or(StoreError::NotFound)?;
        self.get_oauth_client(id).await
    }

    async fn list_oauth_clients_in_realm(
        &self,
        realm_id: Uuid,
    ) -> Result<Vec<OAuthClient>, StoreError> {
        self.list_by_membership(
            keys::client_in_realm_prefix(realm_id),
            keys::client_by_id_key,
            "oauth client",
        )
        .await
    }

    async fn update_oauth_client(
        &self,
        id: Uuid,
        update: OAuthClientUpdate,
    ) -> Result<OAuthClient, StoreError> {
        let by_id = keys::client_by_id_key(id);
        let result: Result<Option<OAuthClient>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let update = update.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(None);
                    };
                    let mut c: OAuthClient = serde_json::from_slice(bytes.as_ref())
                        .map_err(txn_de_err("oauth client"))?;
                    c.name = update.name;
                    c.client_secret_hash = update.client_secret_hash;
                    c.redirect_uris = update.redirect_uris;
                    c.grant_types = update.grant_types;
                    c.pkce_required = update.pkce_required;
                    c.scopes_allowed = update.scopes_allowed;
                    txn_set(&tr, &by_id, &c, "oauth client")?;
                    Ok(Some(c))
                }
            })
            .await;
        opt_to_result(result)
    }

    async fn delete_oauth_client(&self, id: Uuid) -> Result<(), StoreError> {
        let by_id = keys::client_by_id_key(id);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(CreateOutcome::NotFound);
                    };
                    let c: OAuthClient = serde_json::from_slice(bytes.as_ref())
                        .map_err(txn_de_err("oauth client"))?;
                    tr.clear(&by_id);
                    if let Some(cn) = c.bound_to_cn {
                        tr.clear(&keys::client_by_cn_key(cn));
                    }
                    tr.clear(&keys::client_in_realm_key(c.realm_id, id));
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    // ───────────────────────── Upstream connections ───────────────────────

    async fn create_upstream_connection(
        &self,
        req: NewUpstreamConnection,
    ) -> Result<UpstreamConnection, StoreError> {
        let c = UpstreamConnection {
            id: Uuid::new_v4(),
            realm_id: req.realm_id,
            name: req.name,
            kind: req.kind,
            enabled: req.enabled,
            created_at: now(),
        };
        let realm_by_id = keys::realm_by_id_key(c.realm_id);
        let by_id = keys::conn_by_id_key(c.id);
        let by_name = keys::conn_by_realm_name_key(c.realm_id, &c.name);
        let in_realm = keys::conn_in_realm_key(c.realm_id, c.id);
        let id_str = c.id.to_string();

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let c = c.clone();
                let realm_by_id = realm_by_id.clone();
                let by_id = by_id.clone();
                let by_name = by_name.clone();
                let in_realm = in_realm.clone();
                let id_bytes = id_str.as_bytes().to_vec();
                async move {
                    if tr.get(&realm_by_id, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    if tr.get(&by_name, false).await?.is_some() {
                        return Ok(CreateOutcome::Conflict(format!(
                            "connection {:?} already exists in this realm",
                            c.name
                        )));
                    }
                    txn_set(&tr, &by_id, &c, "upstream connection")?;
                    tr.set(&by_name, &id_bytes);
                    tr.set(&in_realm, b"");
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        match outcome {
            Ok(CreateOutcome::Created) => Ok(c),
            Ok(CreateOutcome::Conflict(m)) => Err(StoreError::Conflict(m)),
            Ok(CreateOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_upstream_connection(&self, id: Uuid) -> Result<UpstreamConnection, StoreError> {
        self.read_record(&keys::conn_by_id_key(id), "upstream connection")
            .await
    }

    async fn list_connections_in_realm(
        &self,
        realm_id: Uuid,
    ) -> Result<Vec<UpstreamConnection>, StoreError> {
        self.list_by_membership(
            keys::conn_in_realm_prefix(realm_id),
            keys::conn_by_id_key,
            "upstream connection",
        )
        .await
    }

    async fn set_connection_enabled(
        &self,
        id: Uuid,
        enabled: bool,
    ) -> Result<UpstreamConnection, StoreError> {
        let by_id = keys::conn_by_id_key(id);
        let result: Result<Option<UpstreamConnection>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(None);
                    };
                    let mut c: UpstreamConnection = serde_json::from_slice(bytes.as_ref())
                        .map_err(txn_de_err("upstream connection"))?;
                    // A realm has at most one enabled upstream connection.
                    // Enabling this one disables every other enabled
                    // connection in the same realm in the same txn, so the
                    // identity-source selection is deterministic.
                    if enabled {
                        let realm_pfx = keys::conn_in_realm_prefix(c.realm_id);
                        for other_id in scan_suffix_uuids_in_txn(&tr, &realm_pfx).await? {
                            if other_id == id {
                                continue;
                            }
                            let other_key = keys::conn_by_id_key(other_id);
                            if let Some(ob) = tr.get(&other_key, false).await? {
                                let mut other: UpstreamConnection =
                                    serde_json::from_slice(ob.as_ref())
                                        .map_err(txn_de_err("upstream connection"))?;
                                if other.enabled {
                                    other.enabled = false;
                                    txn_set(&tr, &other_key, &other, "upstream connection")?;
                                }
                            }
                        }
                    }
                    c.enabled = enabled;
                    txn_set(&tr, &by_id, &c, "upstream connection")?;
                    Ok(Some(c))
                }
            })
            .await;
        opt_to_result(result)
    }

    async fn delete_upstream_connection(&self, id: Uuid) -> Result<(), StoreError> {
        let by_id = keys::conn_by_id_key(id);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                async move {
                    let Some(bytes) = tr.get(&by_id, false).await? else {
                        return Ok(CreateOutcome::NotFound);
                    };
                    let c: UpstreamConnection = serde_json::from_slice(bytes.as_ref())
                        .map_err(txn_de_err("upstream connection"))?;
                    tr.clear(&by_id);
                    tr.clear(&keys::conn_by_realm_name_key(c.realm_id, &c.name));
                    tr.clear(&keys::conn_in_realm_key(c.realm_id, id));
                    tr.clear(&keys::claim_mappings_key(id)); // cascade mappings
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn put_claim_mappings(
        &self,
        connection_id: Uuid,
        mappings: Vec<ClaimMapping>,
    ) -> Result<(), StoreError> {
        let conn_by_id = keys::conn_by_id_key(connection_id);
        let map_key = keys::claim_mappings_key(connection_id);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let conn_by_id = conn_by_id.clone();
                let map_key = map_key.clone();
                let mappings = mappings.clone();
                async move {
                    if tr.get(&conn_by_id, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    txn_set(&tr, &map_key, &mappings, "claim mappings")?;
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn list_claim_mappings(
        &self,
        connection_id: Uuid,
    ) -> Result<Vec<ClaimMapping>, StoreError> {
        if self
            .read_bytes(&keys::conn_by_id_key(connection_id))
            .await?
            .is_none()
        {
            return Err(StoreError::NotFound);
        }
        match self.read_bytes(&keys::claim_mappings_key(connection_id)).await? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(de_err("claim mappings")),
            None => Ok(Vec::new()),
        }
    }

    // ───────────────────────────── Signing keys ───────────────────────────

    async fn add_signing_key(
        &self,
        realm_id: Uuid,
        key: NewSigningKey,
    ) -> Result<SigningKey, StoreError> {
        let sk = SigningKey {
            kid: key.kid,
            realm_id,
            alg: key.alg,
            private_pem: key.private_pem,
            public_jwk: key.public_jwk,
            status: key.status,
            not_before: key.not_before,
            not_after: key.not_after,
            created_at: now(),
        };
        let realm_by_id = keys::realm_by_id_key(realm_id);
        let key_key = keys::signing_key_key(realm_id, &sk.kid);

        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let sk = sk.clone();
                let realm_by_id = realm_by_id.clone();
                let key_key = key_key.clone();
                async move {
                    if tr.get(&realm_by_id, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    if tr.get(&key_key, false).await?.is_some() {
                        return Ok(CreateOutcome::Conflict(format!(
                            "kid {:?} already exists in this realm",
                            sk.kid
                        )));
                    }
                    txn_set(&tr, &key_key, &sk, "signing key")?;
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        match outcome {
            Ok(CreateOutcome::Created) => Ok(sk),
            Ok(CreateOutcome::Conflict(m)) => Err(StoreError::Conflict(m)),
            Ok(CreateOutcome::NotFound) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_signing_key(&self, realm_id: Uuid, kid: &str) -> Result<SigningKey, StoreError> {
        self.read_record(&keys::signing_key_key(realm_id, kid), "signing key")
            .await
    }

    async fn list_signing_keys(&self, realm_id: Uuid) -> Result<Vec<SigningKey>, StoreError> {
        if self
            .read_bytes(&keys::realm_by_id_key(realm_id))
            .await?
            .is_none()
        {
            return Err(StoreError::NotFound);
        }
        // The per-realm prefix is scanned in byte-lex order, which (because
        // the kid is the raw trailing segment after a fixed-length realm
        // prefix) yields kid-lex order — the JWKS publish order the suite
        // pins, with no post-sort.
        let bytes = self
            .scan_values(keys::signing_key_prefix(realm_id))
            .await?;
        let mut out = Vec::with_capacity(bytes.len());
        for b in bytes {
            out.push(serde_json::from_slice(&b).map_err(de_err("signing key"))?);
        }
        Ok(out)
    }

    async fn set_signing_key_status(
        &self,
        realm_id: Uuid,
        kid: &str,
        status: KeyStatus,
    ) -> Result<SigningKey, StoreError> {
        let key_key = keys::signing_key_key(realm_id, kid);
        let result: Result<Option<SigningKey>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key_key = key_key.clone();
                async move {
                    let Some(bytes) = tr.get(&key_key, false).await? else {
                        return Ok(None);
                    };
                    let mut sk: SigningKey = serde_json::from_slice(bytes.as_ref())
                        .map_err(txn_de_err("signing key"))?;
                    sk.status = status;
                    txn_set(&tr, &key_key, &sk, "signing key")?;
                    Ok(Some(sk))
                }
            })
            .await;
        opt_to_result(result)
    }

    async fn delete_signing_key(&self, realm_id: Uuid, kid: &str) -> Result<(), StoreError> {
        let key_key = keys::signing_key_key(realm_id, kid);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key_key = key_key.clone();
                async move {
                    if tr.get(&key_key, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    tr.clear(&key_key);
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn try_acquire_rotation_lock(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let lock_key = keys::rotation_lock_key();
        let holder = holder.to_string();
        let acquired: Result<bool, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let lock_key = lock_key.clone();
                let holder = holder.clone();
                async move {
                    if let Some(bytes) = tr.get(&lock_key, false).await? {
                        let lock: RotationLock = serde_json::from_slice(bytes.as_ref())
                            .map_err(txn_de_err("rotation lock"))?;
                        if lock.holder != holder && lock.expires_at > now {
                            return Ok(false);
                        }
                    }
                    let lock = RotationLock {
                        holder: holder.clone(),
                        expires_at,
                    };
                    txn_set(&tr, &lock_key, &lock, "rotation lock")?;
                    Ok(true)
                }
            })
            .await;
        acquired.map_err(StoreError::from)
    }

    async fn release_rotation_lock(&self, holder: &str) -> Result<(), StoreError> {
        let lock_key = keys::rotation_lock_key();
        let holder = holder.to_string();
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let lock_key = lock_key.clone();
                let holder = holder.clone();
                async move {
                    if let Some(bytes) = tr.get(&lock_key, false).await? {
                        let lock: RotationLock = serde_json::from_slice(bytes.as_ref())
                            .map_err(txn_de_err("rotation lock"))?;
                        if lock.holder == holder {
                            tr.clear(&lock_key);
                        }
                    }
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    // ───────────────────────── Short-lived flow records ───────────────────

    async fn put_auth_code(&self, code: AuthCode) -> Result<(), StoreError> {
        let key = keys::auth_code_key(&code.code);
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let code = code.clone();
                async move {
                    txn_set(&tr, &key, &code, "auth code")?;
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    async fn take_auth_code(&self, code: &str) -> Result<AuthCode, StoreError> {
        let key = keys::auth_code_key(code);
        let result: Result<Option<AuthCode>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move {
                    let Some(bytes) = tr.get(&key, false).await? else {
                        return Ok(None);
                    };
                    let ac: AuthCode =
                        serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("auth code"))?;
                    tr.clear(&key);
                    Ok(Some(ac))
                }
            })
            .await;
        match result {
            Ok(Some(ac)) => Ok(ac),
            Ok(None) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn put_refresh_token(&self, token: RefreshToken) -> Result<(), StoreError> {
        let by_id = keys::refresh_by_id_key(token.jti);
        let by_family = keys::refresh_family_key(token.family_id, token.jti);
        let by_user = keys::refresh_user_key(token.user_id, token.jti);
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let by_family = by_family.clone();
                let by_user = by_user.clone();
                let token = token.clone();
                async move {
                    txn_set(&tr, &by_id, &token, "refresh token")?;
                    tr.set(&by_family, b"");
                    tr.set(&by_user, b"");
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    async fn get_refresh_token(&self, jti: Uuid) -> Result<RefreshToken, StoreError> {
        self.read_record(&keys::refresh_by_id_key(jti), "refresh token")
            .await
    }

    async fn revoke_refresh_token(&self, jti: Uuid) -> Result<(), StoreError> {
        let by_id = keys::refresh_by_id_key(jti);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                async move {
                    if tr.get(&by_id, false).await?.is_none() {
                        return Ok(CreateOutcome::NotFound);
                    }
                    revoke_refresh_in_txn(&tr, jti).await?;
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn revoke_refresh_family(&self, family_id: Uuid) -> Result<(), StoreError> {
        let pfx = keys::refresh_family_prefix(family_id);
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let pfx = pfx.clone();
                async move {
                    for jti in scan_suffix_uuids_in_txn(&tr, &pfx).await? {
                        revoke_refresh_in_txn(&tr, jti).await?;
                    }
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    async fn put_device_code(&self, dc: DeviceCode) -> Result<(), StoreError> {
        let by_dc = keys::device_code_by_dc_key(&dc.device_code);
        let by_uc = keys::device_code_by_uc_key(&dc.user_code);
        let dc_str = dc.device_code.clone();
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_dc = by_dc.clone();
                let by_uc = by_uc.clone();
                let dc = dc.clone();
                let dc_bytes = dc_str.as_bytes().to_vec();
                async move {
                    txn_set(&tr, &by_dc, &dc, "device code")?;
                    // by_uc points back at the canonical device_code string so
                    // the by-uc lookup can resolve the by-dc record.
                    tr.set(&by_uc, &dc_bytes);
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    async fn get_device_code_by_dc(&self, device_code: &str) -> Result<DeviceCode, StoreError> {
        self.read_record(&keys::device_code_by_dc_key(device_code), "device code")
            .await
    }

    async fn get_device_code_by_uc(&self, user_code: &str) -> Result<DeviceCode, StoreError> {
        let Some(dc_bytes) = self.read_bytes(&keys::device_code_by_uc_key(user_code)).await? else {
            return Err(StoreError::NotFound);
        };
        let device_code = std::str::from_utf8(&dc_bytes)
            .map_err(|e| StoreError::Backend(format!("device user-code index not utf8: {e}")))?;
        self.get_device_code_by_dc(device_code).await
    }

    async fn update_device_code_status(
        &self,
        device_code: &str,
        status: DeviceCodeStatus,
        user_id: Option<Uuid>,
        granted_tenant: Option<Uuid>,
    ) -> Result<DeviceCode, StoreError> {
        let by_dc = keys::device_code_by_dc_key(device_code);
        let result: Result<Option<DeviceCode>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let by_dc = by_dc.clone();
                async move {
                    let Some(bytes) = tr.get(&by_dc, false).await? else {
                        return Ok(None);
                    };
                    let mut dc: DeviceCode = serde_json::from_slice(bytes.as_ref())
                        .map_err(txn_de_err("device code"))?;
                    dc.status = status;
                    // `None` means "leave unchanged" — the documented contract.
                    if user_id.is_some() {
                        dc.user_id = user_id;
                    }
                    if granted_tenant.is_some() {
                        dc.granted_tenant = granted_tenant;
                    }
                    txn_set(&tr, &by_dc, &dc, "device code")?;
                    Ok(Some(dc))
                }
            })
            .await;
        opt_to_result(result)
    }

    async fn put_session(&self, session: Session) -> Result<(), StoreError> {
        let key = keys::session_by_id_key(session.id);
        let by_user = keys::session_by_user_key(session.user_id, session.id);
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let by_user = by_user.clone();
                let session = session.clone();
                async move {
                    txn_set(&tr, &key, &session, "session")?;
                    tr.set(&by_user, b""); // reverse index: user → session
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    async fn get_session(&self, id: Uuid) -> Result<Session, StoreError> {
        self.read_record(&keys::session_by_id_key(id), "session")
            .await
    }

    async fn delete_session(&self, id: Uuid) -> Result<(), StoreError> {
        let key = keys::session_by_id_key(id);
        let outcome: Result<CreateOutcome, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move {
                    let Some(bytes) = tr.get(&key, false).await? else {
                        return Ok(CreateOutcome::NotFound);
                    };
                    let session: Session =
                        serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("session"))?;
                    tr.clear(&key);
                    // Clear the reverse index alongside the record.
                    tr.clear(&keys::session_by_user_key(session.user_id, id));
                    Ok(CreateOutcome::Created)
                }
            })
            .await;
        finish(outcome)
    }

    async fn put_broker_state(&self, st: BrokerState) -> Result<(), StoreError> {
        let key = keys::broker_state_key(&st.state);
        let result: Result<(), FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                let st = st.clone();
                async move {
                    txn_set(&tr, &key, &st, "broker state")?;
                    Ok(())
                }
            })
            .await;
        result.map_err(StoreError::from)
    }

    async fn take_broker_state(&self, state: &str) -> Result<BrokerState, StoreError> {
        let key = keys::broker_state_key(state);
        let result: Result<Option<BrokerState>, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let key = key.clone();
                async move {
                    let Some(bytes) = tr.get(&key, false).await? else {
                        return Ok(None);
                    };
                    let bs: BrokerState = serde_json::from_slice(bytes.as_ref())
                        .map_err(txn_de_err("broker state"))?;
                    tr.clear(&key);
                    Ok(Some(bs))
                }
            })
            .await;
        match result {
            Ok(Some(bs)) => Ok(bs),
            Ok(None) => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<usize, StoreError> {
        // Drop every short-lived record whose `expires_at < now`. Each
        // record family is swept by scanning its primary range, decoding,
        // and clearing the expired ones (plus their indices). The contract
        // (and the suite) keep records with `expires_at == now`.
        let mut removed = 0usize;

        // Auth codes.
        removed += self
            .sweep_family::<AuthCode, _>(keys::auth_code_prefix(), now, |tr, ac| {
                tr.clear(&keys::auth_code_key(&ac.code));
            })
            .await?;

        // Refresh tokens (clear by_id + family + user indices).
        removed += self
            .sweep_family::<RefreshToken, _>(keys::refresh_by_id_prefix(), now, |tr, t| {
                tr.clear(&keys::refresh_by_id_key(t.jti));
                tr.clear(&keys::refresh_family_key(t.family_id, t.jti));
                tr.clear(&keys::refresh_user_key(t.user_id, t.jti));
            })
            .await?;

        // Device codes (clear by_dc + by_uc).
        removed += self
            .sweep_family::<DeviceCode, _>(keys::device_code_by_dc_prefix(), now, |tr, d| {
                tr.clear(&keys::device_code_by_dc_key(&d.device_code));
                tr.clear(&keys::device_code_by_uc_key(&d.user_code));
            })
            .await?;

        // Sessions (clear by_id + by_user index).
        removed += self
            .sweep_family::<Session, _>(keys::session_by_id_prefix(), now, |tr, s| {
                tr.clear(&keys::session_by_id_key(s.id));
                tr.clear(&keys::session_by_user_key(s.user_id, s.id));
            })
            .await?;

        // Broker states.
        removed += self
            .sweep_family::<BrokerState, _>(keys::broker_state_prefix(), now, |tr, bs| {
                tr.clear(&keys::broker_state_key(&bs.state));
            })
            .await?;

        Ok(removed)
    }
}

// ── Free functions shared across methods ──────────────────────────────

impl FdbStore {
    /// Sweep one record family: scan `prefix`, decode each value as `T`,
    /// and clear (via `clear_fn`) the ones with `expires_at < now`. Returns
    /// how many were removed. `T` must expose `expires_at` via the
    /// [`HasExpiry`] trait.
    async fn sweep_family<T, F>(
        &self,
        prefix: Vec<u8>,
        now: DateTime<Utc>,
        clear_fn: F,
    ) -> Result<usize, StoreError>
    where
        T: serde::de::DeserializeOwned + HasExpiry + Clone + Send + Sync + 'static,
        F: Fn(&Transaction, &T) + Copy + Send + Sync + 'static,
    {
        let removed: Result<usize, FdbBindingError> = self
            .db
            .run(|tr, _| {
                let prefix = prefix.clone();
                async move {
                    // Drain the whole family across batches; a sweep that
                    // stopped at the first batch would leave expired records
                    // (and their indices) behind.
                    let values = drain_range_values(&tr, &prefix).await?;
                    let mut n = 0usize;
                    for bytes in values {
                        let v: T = serde_json::from_slice(&bytes)
                            .map_err(txn_de_err("sweep record"))?;
                        if v.expires_at() < now {
                            clear_fn(&tr, &v);
                            n += 1;
                        }
                    }
                    Ok(n)
                }
            })
            .await;
        removed.map_err(StoreError::from)
    }
}

/// Minimal accessor so `sweep_family` can read `expires_at` generically.
trait HasExpiry {
    fn expires_at(&self) -> DateTime<Utc>;
}
impl HasExpiry for AuthCode {
    fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}
impl HasExpiry for RefreshToken {
    fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}
impl HasExpiry for DeviceCode {
    fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}
impl HasExpiry for Session {
    fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}
impl HasExpiry for BrokerState {
    fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

/// Structural-scope rule for role assignments. Identical to MemStore's
/// `check_grant_scope`; duplicated here so the FDB backend doesn't depend
/// on a `pub(crate)` from `mem`.
fn check_grant_scope(realm_scope: &RealmScope, target: &AssignmentTarget) -> Result<(), String> {
    use AssignmentTarget as T;
    use RealmScope as S;
    match (realm_scope, target) {
        (S::System, T::Fleet) => Ok(()),
        (S::System, _) => Err("the System realm may only grant Fleet-scoped roles".into()),
        (_, T::Fleet) => Err("only the System realm may grant Fleet-scoped roles".into()),
        (S::Silo { silo_id: rs }, T::Silo { silo_id: ts }) if rs == ts => Ok(()),
        (S::Silo { .. }, T::Silo { .. }) => {
            Err("a silo realm may only grant roles over its own silo".into())
        }
        (S::Silo { .. }, T::Tenant { .. }) => Ok(()),
        (S::Tenant { tenant_id: rt }, T::Tenant { tenant_id: tt }) if rt == tt => Ok(()),
        (S::Tenant { .. }, _) => {
            Err("a tenant realm may only grant roles over its own tenant".into())
        }
    }
}

/// True if the subject record (a serialized `User` or `Group`) belongs to
/// `realm_id`.
fn subject_in_realm(
    subject: &AssignmentSubject,
    record_bytes: &[u8],
    realm_id: Uuid,
) -> Result<bool, serde_json::Error> {
    match subject {
        AssignmentSubject::User { .. } => {
            let u: User = serde_json::from_slice(record_bytes)?;
            Ok(u.realm_id == realm_id)
        }
        AssignmentSubject::Group { .. } => {
            let g: Group = serde_json::from_slice(record_bytes)?;
            Ok(g.realm_id == realm_id)
        }
    }
}

/// Mark a refresh token revoked, in-txn. No-op if the token is gone.
async fn revoke_refresh_in_txn(tr: &Transaction, jti: Uuid) -> Result<(), FdbBindingError> {
    let by_id = keys::refresh_by_id_key(jti);
    if let Some(bytes) = tr.get(&by_id, false).await? {
        let mut t: RefreshToken =
            serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("refresh token"))?;
        t.revoked = true;
        txn_set(tr, &by_id, &t, "refresh token")?;
    }
    Ok(())
}

/// Clear a refresh token + its family/user indices, in-txn.
async fn clear_refresh_in_txn(tr: &Transaction, jti: Uuid) -> Result<(), FdbBindingError> {
    let by_id = keys::refresh_by_id_key(jti);
    if let Some(bytes) = tr.get(&by_id, false).await? {
        let t: RefreshToken =
            serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("refresh token"))?;
        tr.clear(&by_id);
        tr.clear(&keys::refresh_family_key(t.family_id, jti));
        tr.clear(&keys::refresh_user_key(t.user_id, jti));
    }
    Ok(())
}

/// Clear a role assignment + its by_subject/by_target/dup indices, in-txn.
async fn clear_role_assignment_in_txn(tr: &Transaction, id: Uuid) -> Result<(), FdbBindingError> {
    let by_id = keys::role_by_id_key(id);
    if let Some(bytes) = tr.get(&by_id, false).await? {
        let a: RoleAssignment =
            serde_json::from_slice(bytes.as_ref()).map_err(txn_de_err("role assignment"))?;
        tr.clear(&by_id);
        tr.clear(&keys::role_by_subject_key(&a.subject, id));
        tr.clear(&keys::role_by_target_key(&a.target, id));
        tr.clear(&keys::role_dup_key(a.realm_id, &a.subject, &a.target, a.role));
    }
    Ok(())
}

/// Scan a membership prefix inside a txn, returning the trailing uuid of
/// every key. Drains the whole range across batches so a cascade never
/// misses members past the first batch.
async fn scan_suffix_uuids_in_txn(
    tr: &Transaction,
    prefix: &[u8],
) -> Result<Vec<Uuid>, FdbBindingError> {
    let plen = prefix.len();
    let mut suffixes: Vec<String> = Vec::new();
    drain_range(tr, prefix, |kv| {
        // Stash the suffix bytes; UTF-8 / uuid parsing happens after the
        // borrow on `kv` ends so the visitor stays infallible.
        suffixes.push(String::from_utf8_lossy(&kv.key()[plen..]).into_owned());
    })
    .await?;
    let mut out = Vec::with_capacity(suffixes.len());
    for s in suffixes {
        out.push(Uuid::parse_str(&s).map_err(txn_err("index suffix not uuid"))?);
    }
    Ok(out)
}

/// True if `prefix` has no keys (a single-row probe).
async fn range_is_empty(tr: &Transaction, prefix: &[u8]) -> Result<bool, FdbBindingError> {
    let (begin, end) = prefix_range(prefix);
    let opt = RangeOption {
        begin: KeySelector::first_greater_or_equal(begin),
        end: KeySelector::first_greater_or_equal(end),
        limit: Some(1),
        ..RangeOption::default()
    };
    let kvs = tr.get_range(&opt, 1, false).await?;
    Ok(kvs.is_empty())
}

/// Map a generic create/delete outcome back to `StoreError`.
fn finish(outcome: Result<CreateOutcome, FdbBindingError>) -> Result<(), StoreError> {
    match outcome {
        Ok(CreateOutcome::Created) => Ok(()),
        Ok(CreateOutcome::Conflict(m)) => Err(StoreError::Conflict(m)),
        Ok(CreateOutcome::NotFound) => Err(StoreError::NotFound),
        Err(e) => Err(e.into()),
    }
}

/// Map an `Ok(Option<T>)` update result to `T` / `NotFound`.
fn opt_to_result<T>(result: Result<Option<T>, FdbBindingError>) -> Result<T, StoreError> {
    match result {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(StoreError::NotFound),
        Err(e) => Err(e.into()),
    }
}
