// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Domain types shared between the storage layer and the wire surface.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A tenancy boundary. Every Triton Cloud resource ultimately rolls up
/// to a silo; in production each silo is bound to its own identity
/// provider. Phase 0 carries only the bare-minimum identifying fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Silo {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a silo.
///
/// Distinct from [`Silo`] because the server assigns `id` and
/// `created_at`. `description` is optional on the wire and stored as
/// an empty string when omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewSilo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Operator account. Phase 0 has exactly one role bit (`is_root`)
/// because Cedar authorisation is still expressed against the root
/// principal. Once richer policies land, this grows to carry group
/// membership, silo binding, and the federation source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    /// Bcrypt hash of the operator's password.
    pub password_hash: String,
    /// True for the bootstrap operator. Cedar policy uses this to
    /// short-circuit to "permit anything" until per-action policies
    /// are written.
    pub is_root: bool,
    pub created_at: DateTime<Utc>,
}

/// Wire-safe view of a [`User`]: same identity, no credential material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UserView {
    pub id: Uuid,
    pub username: String,
    pub is_root: bool,
    pub created_at: DateTime<Utc>,
}

impl From<User> for UserView {
    fn from(user: User) -> Self {
        UserView {
            id: user.id,
            username: user.username,
            is_root: user.is_root,
            created_at: user.created_at,
        }
    }
}

/// API key record. Storage carries the bcrypt hash of the secret
/// segment plus the non-secret `lookup_id` used to find the record
/// in O(1). Plaintext is shown to the operator exactly once at
/// creation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: Uuid,
    /// User this key authenticates as.
    pub user_id: Uuid,
    /// Operator-supplied free-text label (e.g. "ci-pipeline").
    pub description: String,
    /// Non-secret 12-character lookup identifier — the prefix half of
    /// the wire-form secret. Indexed by `Store` for O(1) lookup.
    pub lookup_id: String,
    /// Bcrypt hash of the secret segment of the wire-form key.
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

/// Wire-safe view of an [`ApiKey`]: identifying metadata, no hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

impl From<ApiKey> for ApiKeyView {
    fn from(key: ApiKey) -> Self {
        ApiKeyView {
            id: key.id,
            user_id: key.user_id,
            description: key.description,
            created_at: key.created_at,
        }
    }
}

/// Cluster-level system keys. Phase 0 has exactly one
/// (`SystemKey::JwtSigning`); future entries will include the
/// transit-engine master key and any per-silo OIDC client secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SystemKey {
    /// 32-byte HS256 secret used to sign and validate operator JWTs.
    JwtSigning,
}

impl SystemKey {
    /// Stable storage tag, used as the FDB key suffix.
    pub fn tag(self) -> &'static str {
        match self {
            SystemKey::JwtSigning => "jwt_signing",
        }
    }
}
