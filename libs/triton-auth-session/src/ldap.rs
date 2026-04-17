// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! UFDS authentication over LDAP.
//!
//! Binds as the configured admin, looks up the user as an `sdcperson`
//! entry, and verifies the supplied password with an LDAP `compare` on
//! `userPassword`. Roles are derived from `memberof` DNs; when the server
//! doesn't populate `memberof`, we fall back to a `groupofuniquenames`
//! search under `ou=groups, o=smartdc`.

use crate::error::{SessionError, SessionResult};
use crate::models::Role;
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, Scope, SearchEntry};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct LdapConfig {
    pub url: Url,
    pub bind_dn: String,
    pub bind_password: SecretString,
    pub search_base: String,
    pub tls_verify: bool,
    pub connection_timeout_secs: NonZeroU64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UfdsUser {
    pub uuid: Uuid,
    pub login: String,
    pub email: Option<String>,
    pub cn: Option<String>,
    pub company: Option<String>,
    pub groups: Vec<String>,
    /// Typed roles extracted from LDAP group membership.
    pub roles: Vec<Role>,
    dn: String,
}

impl UfdsUser {
    /// The full DN of the user's LDAP entry. Exposed for callers that
    /// need to perform further LDAP operations scoped to the user.
    pub fn dn(&self) -> &str {
        &self.dn
    }
}

pub struct LdapService {
    config: Arc<RwLock<LdapConfig>>,
}

/// Escape a value for use in an LDAP search filter per RFC 4515 §3.
fn ldap_escape_filter(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\5c"),
            '*' => escaped.push_str("\\2a"),
            '(' => escaped.push_str("\\28"),
            ')' => escaped.push_str("\\29"),
            '\0' => escaped.push_str("\\00"),
            _ => escaped.push(c),
        }
    }
    escaped
}

impl LdapService {
    pub fn new(config: LdapConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
        }
    }

    pub async fn authenticate(&self, username: &str, password: &str) -> SessionResult<UfdsUser> {
        let mut ldap = self.connect().await?;
        self.bind_admin(&mut ldap).await?;

        let user = self.find_user(&mut ldap, username).await?;

        let password_valid = self.verify_password(&mut ldap, &user.dn, password).await?;
        if !password_valid {
            return Err(SessionError::AuthenticationFailed);
        }

        // arch-lint: allow(no-error-swallowing) reason="Unbind failure is non-fatal; connection is being dropped"
        if let Err(e) = ldap.unbind().await {
            warn!("LDAP unbind failed after authentication: {e}");
        }
        Ok(user)
    }

    async fn connect(&self) -> SessionResult<Ldap> {
        let config = self.config.read().await;
        let ldap_url = config.url.as_str();

        let settings = LdapConnSettings::new()
            .set_conn_timeout(Duration::from_secs(config.connection_timeout_secs.get()))
            .set_no_tls_verify(!config.tls_verify)
            .set_starttls(false);

        let (conn, ldap) = LdapConnAsync::with_settings(settings, ldap_url)
            .await
            .map_err(|e| {
                error!("Failed to connect to LDAP: {e}");
                SessionError::LdapUnavailable(format!("LDAP connection failed: {e}"))
            })?;

        tokio::spawn(async move {
            // arch-lint: allow(no-error-swallowing) reason="Background task cannot propagate; logging is the only viable action"
            if let Err(e) = conn.drive().await {
                error!("LDAP connection error: {e}");
            }
        });

        Ok(ldap)
    }

    async fn bind_admin(&self, ldap: &mut Ldap) -> SessionResult<()> {
        let config = self.config.read().await;
        let result = ldap
            .simple_bind(&config.bind_dn, config.bind_password.expose_secret())
            .await
            .map_err(|e| {
                error!("Admin bind error: {e}");
                SessionError::LdapConfigError(format!("LDAP admin bind error: {e}"))
            })?;

        if result.rc != 0 {
            error!("Admin bind failed with code: {}", result.rc);
            return Err(SessionError::LdapConfigError(
                "LDAP admin bind failed".to_string(),
            ));
        }
        Ok(())
    }

    async fn find_user(&self, ldap: &mut Ldap, username: &str) -> SessionResult<UfdsUser> {
        let config = self.config.read().await;
        let search_base = &config.search_base;
        let safe_username = ldap_escape_filter(username);
        let filter = format!("(&(objectclass=sdcperson)(login={safe_username}))");

        info!("Searching for user {username} in {search_base}");

        let (rs, _) = ldap
            .search(
                search_base,
                Scope::Subtree,
                &filter,
                vec!["dn", "uuid", "login", "email", "cn", "company", "memberof"],
            )
            .await
            .map_err(|e| {
                error!("LDAP search error: {e}");
                SessionError::LdapUnavailable(format!("LDAP search failed: {e}"))
            })?
            .success()
            .map_err(|e| {
                warn!(username = %username, "LDAP search result error: {e}");
                SessionError::AuthenticationFailed
            })?;

        if rs.is_empty() {
            debug!("User not found: {username}");
            return Err(SessionError::AuthenticationFailed);
        }

        let entry = SearchEntry::construct(
            rs.into_iter()
                .next()
                .ok_or(SessionError::AuthenticationFailed)?,
        );
        drop(config);
        let mut user = self.parse_user_entry(entry)?;

        // Fall back to groupofuniquenames search if memberof was empty.
        // Reuses the same bound LDAP handle rather than opening a second
        // connection — simpler and avoids doubling per-request LDAP cost.
        if user.groups.is_empty() {
            match self.check_user_groups(ldap, &user.uuid).await {
                Ok(group_names) => {
                    user.roles = group_names.iter().map(|g| Role::from(g.as_str())).collect();
                    user.groups = group_names;
                }
                Err(e) => {
                    error!(
                        user = %user.login,
                        "Group lookup failed, cannot determine admin status: {e}"
                    );
                    return Err(e);
                }
            }
        }

        Ok(user)
    }

    /// Look up the groups the user belongs to by listing every group under
    /// `ou=groups, o=smartdc` and matching the user's UUID against each
    /// group's `uniquemember` values in-process.
    ///
    /// We deliberately do not push the match into the LDAP filter (as
    /// `(uniquemember=<dn>)`) because that requires encoding a DN as a
    /// filter value, and ldap3's filter-string parser treats embedded `=`
    /// signs in a way that UFDS does not match -- the same filter that
    /// works via `sdc-ldap` returns zero hits through ldap3. Since the DC
    /// has a handful of groups, the server-side listing is cheap and the
    /// client-side UUID match is exact.
    async fn check_user_groups(
        &self,
        ldap: &mut Ldap,
        user_uuid: &Uuid,
    ) -> SessionResult<Vec<String>> {
        let filter = "(objectClass=groupofuniquenames)";
        debug!(user_uuid = %user_uuid, "group membership search");

        let (rs, _) = ldap
            .search(
                "ou=groups, o=smartdc",
                Scope::Subtree,
                filter,
                vec!["cn", "uniquemember"],
            )
            .await
            .map_err(|e| SessionError::LdapUnavailable(format!("LDAP group search failed: {e}")))?
            .success()
            .map_err(|e| {
                warn!(user_uuid = %user_uuid, "LDAP group search result error: {e}");
                SessionError::LdapUnavailable("Group search failed".to_string())
            })?;

        debug!(raw_entries = rs.len(), "group search returned LDAP entries");
        let user_uuid_str = user_uuid.to_string();
        let groups: Vec<String> = rs
            .into_iter()
            .filter_map(|entry| {
                let entry = SearchEntry::construct(entry);
                debug!(dn = %entry.dn, attrs = ?entry.attrs, "group entry");
                let cn = entry.attrs.get("cn")?.first()?.clone();
                let members = entry.attrs.get("uniquemember")?;
                members
                    .iter()
                    .any(|m| m.contains(&user_uuid_str))
                    .then_some(cn)
            })
            .collect();
        debug!(groups = ?groups, "group membership search result");

        Ok(groups)
    }

    fn parse_user_entry(&self, entry: SearchEntry) -> SessionResult<UfdsUser> {
        let attrs = &entry.attrs;
        let get =
            |name: &str| -> Option<String> { attrs.get(name).and_then(|v| v.first()).cloned() };

        let uuid_str = get("uuid").ok_or_else(|| {
            error!(dn = %entry.dn, "LDAP entry missing 'uuid' attribute");
            SessionError::Internal("UFDS entry missing uuid".to_string())
        })?;
        let uuid = Uuid::parse_str(&uuid_str).map_err(|e| {
            error!(dn = %entry.dn, uuid = %uuid_str, "Invalid UUID in LDAP entry: {e}");
            SessionError::Internal(format!("UFDS uuid parse: {e}"))
        })?;

        // arch-lint: allow(no-silent-result-drop) reason="Option::unwrap_or_default — missing memberof attribute is a valid empty state"
        let memberof = attrs.get("memberof").cloned().unwrap_or_default();
        let roles: Vec<Role> = memberof
            .iter()
            .filter_map(|group| {
                group
                    .split(',')
                    .find(|part| part.trim().starts_with("cn="))
                    .and_then(|cn| cn.trim().strip_prefix("cn="))
                    .map(Role::from)
            })
            .collect();
        let login = get("login").ok_or_else(|| {
            error!(dn = %entry.dn, "LDAP entry missing 'login' attribute");
            SessionError::Internal("UFDS entry missing login".to_string())
        })?;

        Ok(UfdsUser {
            dn: entry.dn,
            uuid,
            login,
            email: get("email"),
            cn: get("cn"),
            company: get("company"),
            groups: memberof,
            roles,
        })
    }

    async fn verify_password(
        &self,
        ldap: &mut Ldap,
        user_dn: &str,
        password: &str,
    ) -> SessionResult<bool> {
        match ldap.compare(user_dn, "userPassword", password).await {
            // LDAP result code 6 = compareTrue (RFC 4511 §4.10).
            Ok(result) => Ok(result.0.rc == 6),
            Err(e) => {
                error!(user_dn = %user_dn, "Password verification error: {e}");
                Err(SessionError::LdapUnavailable(format!(
                    "password verify: {e}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> LdapConfig {
        LdapConfig {
            url: Url::parse("ldap://localhost:389").unwrap(),
            bind_dn: "cn=admin".to_string(),
            bind_password: SecretString::from("password"),
            search_base: "ou=users, o=smartdc".to_string(),
            tls_verify: false,
            connection_timeout_secs: NonZeroU64::new(10).unwrap(),
        }
    }

    #[test]
    fn ldap_escape_filter_handles_special_chars() {
        assert_eq!(ldap_escape_filter("admin"), "admin");
        assert_eq!(ldap_escape_filter("user*"), "user\\2a");
        assert_eq!(
            ldap_escape_filter("user)(cn=*))(|(cn=*"),
            "user\\29\\28cn=\\2a\\29\\29\\28|\\28cn=\\2a"
        );
        assert_eq!(ldap_escape_filter("back\\slash"), "back\\5cslash");
        assert_eq!(ldap_escape_filter("nul\0byte"), "nul\\00byte");
    }

    #[test]
    fn roles_extracted_from_memberof_dns() {
        let service = LdapService::new(test_config());

        let mut attrs = std::collections::HashMap::new();
        attrs.insert(
            "uuid".to_string(),
            vec!["550e8400-e29b-41d4-a716-446655440000".to_string()],
        );
        attrs.insert("login".to_string(), vec!["testuser".to_string()]);
        attrs.insert(
            "memberof".to_string(),
            vec![
                "cn=operators,ou=groups,o=smartdc".to_string(),
                "cn=readers,ou=groups,o=smartdc".to_string(),
            ],
        );

        let entry = SearchEntry {
            dn: "uuid=550e8400-e29b-41d4-a716-446655440000,ou=users,o=smartdc".to_string(),
            attrs,
            bin_attrs: std::collections::HashMap::new(),
        };

        let user = service.parse_user_entry(entry).unwrap();
        assert_eq!(user.roles, vec![Role::Operators, Role::Unknown]);
        assert!(user.groups[0].contains("ou=groups"));
    }
}
