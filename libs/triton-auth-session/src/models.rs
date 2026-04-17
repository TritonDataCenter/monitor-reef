// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Claims and role model embedded in session tokens.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Typed roles derived from LDAP group membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Operators,
    Admins,
    #[serde(other)]
    Unknown,
}

impl From<&str> for Role {
    fn from(s: &str) -> Self {
        match s {
            "operators" => Role::Operators,
            "admins" => Role::Admins,
            _ => Role::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// JWT subject — the user's UUID.
    pub sub: Uuid,
    pub username: String,
    pub(crate) roles: Vec<Role>,
    /// Serialized into the JWT for backward compatibility with verifiers
    /// that read it directly. Do NOT read this field to make authorization
    /// decisions — call `is_admin()` instead, which derives the answer
    /// from `roles` and cannot be forged.
    #[serde(rename = "is_admin")]
    pub(crate) is_admin_claim: bool,
    pub exp: i64,
    pub iat: i64,
    pub jti: String,
}

impl Claims {
    pub fn new(
        sub: Uuid,
        username: String,
        roles: Vec<Role>,
        exp: i64,
        iat: i64,
        jti: String,
    ) -> Self {
        let is_admin_claim = roles_imply_admin(&roles);
        Self {
            sub,
            username,
            roles,
            is_admin_claim,
            exp,
            iat,
            jti,
        }
    }

    #[cfg(test)]
    pub fn new_for_test(
        sub: Uuid,
        username: String,
        roles: Vec<Role>,
        is_admin_claim: bool,
        exp: i64,
        iat: i64,
        jti: String,
    ) -> Self {
        Self {
            sub,
            username,
            roles,
            is_admin_claim,
            exp,
            iat,
            jti,
        }
    }

    pub fn user_uuid(&self) -> Uuid {
        self.sub
    }

    pub fn roles(&self) -> &[Role] {
        &self.roles
    }

    /// Authoritative admin check — derives from `roles`, not from the
    /// serialized `is_admin` field which an attacker could forge.
    pub fn is_admin(&self) -> bool {
        roles_imply_admin(&self.roles)
    }
}

pub fn roles_imply_admin(roles: &[Role]) -> bool {
    roles
        .iter()
        .any(|r| matches!(r, Role::Operators | Role::Admins))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_claims(roles: Vec<Role>) -> Claims {
        Claims::new(
            Uuid::new_v4(),
            "testuser".to_string(),
            roles,
            0,
            0,
            Uuid::new_v4().to_string(),
        )
    }

    #[test]
    fn is_admin_true_for_operators() {
        assert!(make_claims(vec![Role::Operators]).is_admin());
    }

    #[test]
    fn is_admin_true_for_admins() {
        assert!(make_claims(vec![Role::Admins]).is_admin());
    }

    #[test]
    fn is_admin_true_for_mixed_roles() {
        assert!(make_claims(vec![Role::Unknown, Role::Admins]).is_admin());
    }

    #[test]
    fn is_admin_false_for_unknown_only() {
        assert!(!make_claims(vec![Role::Unknown]).is_admin());
    }

    #[test]
    fn is_admin_false_for_empty_roles() {
        assert!(!make_claims(vec![]).is_admin());
    }

    #[test]
    fn is_admin_method_ignores_stored_field() {
        let claims = Claims::new_for_test(
            Uuid::new_v4(),
            "testuser".to_string(),
            vec![Role::Unknown],
            true,
            0,
            0,
            Uuid::new_v4().to_string(),
        );
        assert!(!claims.is_admin());
    }

    #[test]
    fn role_from_str_matches_serde_deserialization() {
        let variants = ["operators", "admins"];
        for name in variants {
            let from_str = Role::from(name);
            let from_serde: Role = serde_json::from_str(&format!("\"{name}\"")).unwrap();
            assert_eq!(from_str, from_serde, "Role mapping drift for '{name}'");
        }

        let from_str = Role::from("bogus");
        let from_serde: Role = serde_json::from_str("\"bogus\"").unwrap();
        assert_eq!(from_str, from_serde);
    }

    #[test]
    fn user_uuid_returns_sub() {
        let id = Uuid::new_v4();
        let claims = Claims::new(
            id,
            "testuser".to_string(),
            vec![],
            0,
            0,
            Uuid::new_v4().to_string(),
        );
        assert_eq!(claims.user_uuid(), id);
    }
}
