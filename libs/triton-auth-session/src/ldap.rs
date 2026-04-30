// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! UFDS password verification over LDAP.
//!
//! Binds as the configured admin, looks up the user as an `sdcperson`
//! entry, and verifies the supplied password with an LDAP `compare` on
//! `userPassword`. Group membership / operator status is resolved
//! separately from the Mahi auth cache (see [`crate::mahi`]).

use crate::error::{SessionError, SessionResult};
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, Scope, SearchEntry, SearchResult};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{error, info, warn};
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

/// Minimal identity returned by a successful LDAP bind. Authoritative
/// attributes (operator status, group memberships, display metadata) are
/// fetched from mahi after this point; `UfdsUser` only carries what is
/// needed to correlate that lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UfdsUser {
    pub uuid: Uuid,
    pub login: String,
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
                vec!["dn", "uuid", "login"],
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
            return Err(SessionError::AuthenticationFailed);
        }

        let entry = SearchEntry::construct(
            rs.into_iter()
                .next()
                .ok_or(SessionError::AuthenticationFailed)?,
        );
        drop(config);
        self.parse_user_entry(entry)
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
        let login = get("login").ok_or_else(|| {
            error!(dn = %entry.dn, "LDAP entry missing 'login' attribute");
            SessionError::Internal("UFDS entry missing login".to_string())
        })?;

        Ok(UfdsUser {
            dn: entry.dn,
            uuid,
            login,
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

    /// Read a single value from a UFDS capimetadata entry.
    ///
    /// Looks up `metadata=<namespace>, uuid=<user_uuid>, <search_base>`
    /// (the canonical SmartDC capi-metadata DN layout) with a base-scoped
    /// search and returns the JSON-decoded value of the requested
    /// attribute `key`.
    ///
    /// Returns `Ok(None)` when the metadata entry does not exist
    /// (LDAP `noSuchObject`, rc=32) or when the requested key is
    /// absent from an entry that does exist — both are common
    /// "no metadata of this kind" cases. Returns `Err` on connection
    /// or protocol failures, and on stored values that fail to parse
    /// as JSON.
    ///
    /// `namespace` and `key` are interpolated without LDAP-special
    /// escaping; both are expected to be trusted constants chosen by
    /// callers (e.g. `"portal"` / `"usemoresecurity"`). The UUID
    /// renders as lowercase hex via `Display`, which is safe in DN
    /// position.
    pub async fn read_user_metadata_value(
        &self,
        user_uuid: Uuid,
        namespace: &str,
        key: &str,
    ) -> SessionResult<Option<serde_json::Value>> {
        let mut ldap = self.connect().await?;
        self.bind_admin(&mut ldap).await?;

        let dn = {
            let config = self.config.read().await;
            format!(
                "metadata={}, uuid={}, {}",
                namespace, user_uuid, config.search_base
            )
        };

        let search = ldap
            .search(&dn, Scope::Base, "(objectclass=capimetadata)", vec![key])
            .await;

        // arch-lint: allow(no-error-swallowing) reason="Unbind failure is non-fatal; connection is being dropped"
        if let Err(e) = ldap.unbind().await {
            warn!("LDAP unbind failed after metadata read: {e}");
        }

        let SearchResult(rs, res) = search.map_err(|e| {
            error!(dn = %dn, "LDAP metadata search error: {e}");
            SessionError::LdapUnavailable(format!("LDAP metadata search failed: {e}"))
        })?;

        match res.rc {
            0 => {
                let Some(raw_entry) = rs.into_iter().next() else {
                    return Ok(None);
                };
                let entry = SearchEntry::construct(raw_entry);
                extract_metadata_value(&entry, key)
            }
            // 32 = noSuchObject. The user has no capimetadata at all,
            // or no metadata under this namespace. Treat as
            // "not present" — that's the common case for users who
            // are not enrolled in 2FA.
            32 => Ok(None),
            rc => {
                error!(dn = %dn, rc, "LDAP metadata search non-success: {}", res.text);
                Err(SessionError::LdapUnavailable(format!(
                    "LDAP metadata search rc={rc}: {}",
                    res.text
                )))
            }
        }
    }

    /// Read the TOTP shared secret for `user_uuid` from the UFDS
    /// portal namespace, returning the base32-encoded secret if the
    /// user is enrolled in two-factor authentication.
    ///
    /// The DN, namespace, and metadata key are all the ones piranha
    /// writes to today (`metadata=portal,…`, attribute
    /// `usemoresecurity`, JSON payload `{"secretkey": "<BASE32>"}`),
    /// so any existing piranha enrollment is honoured without
    /// migration. See the project memory note
    /// `project_tritonapi_2fa_namespace.md` for the namespace-sharing
    /// decision.
    ///
    /// `Ok(None)` covers all of: no capimetadata entry, no
    /// `usemoresecurity` attribute, missing `secretkey` field, and
    /// empty `secretkey` value. The last two address the disable
    /// path in piranha, which clears the secret rather than removing
    /// the entry.
    pub async fn read_totp_secret(&self, user_uuid: Uuid) -> SessionResult<Option<String>> {
        let metadata = match self
            .read_user_metadata_value(user_uuid, "portal", "usemoresecurity")
            .await?
        {
            Some(v) => v,
            None => return Ok(None),
        };
        Ok(extract_secretkey(&metadata))
    }
}

fn extract_metadata_value(
    entry: &SearchEntry,
    key: &str,
) -> SessionResult<Option<serde_json::Value>> {
    let Some(raw) = entry.attrs.get(key).and_then(|v| v.first()) else {
        return Ok(None);
    };
    serde_json::from_str(raw).map(Some).map_err(|e| {
        SessionError::Internal(format!(
            "UFDS metadata attribute {key:?} is not valid JSON: {e}"
        ))
    })
}

fn extract_secretkey(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .get("secretkey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
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
    fn parse_user_entry_extracts_uuid_and_login() {
        let service = LdapService::new(test_config());

        let mut attrs = std::collections::HashMap::new();
        attrs.insert(
            "uuid".to_string(),
            vec!["550e8400-e29b-41d4-a716-446655440000".to_string()],
        );
        attrs.insert("login".to_string(), vec!["testuser".to_string()]);

        let dn = "uuid=550e8400-e29b-41d4-a716-446655440000,ou=users,o=smartdc".to_string();
        let entry = SearchEntry {
            dn: dn.clone(),
            attrs,
            bin_attrs: std::collections::HashMap::new(),
        };

        let user = service.parse_user_entry(entry).unwrap();
        assert_eq!(user.login, "testuser");
        assert_eq!(user.dn(), dn);
        assert_eq!(
            user.uuid,
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
    }

    /// Build a `SearchEntry` carrying the given attribute name + raw
    /// value, with an arbitrary DN. Used by the metadata-extraction
    /// tests below — they exercise pure parsing logic, the DN is just
    /// a label.
    fn metadata_entry(attr: &str, value: &str) -> SearchEntry {
        let mut attrs = std::collections::HashMap::new();
        attrs.insert(attr.to_string(), vec![value.to_string()]);
        SearchEntry {
            dn: "metadata=portal, uuid=00000000-0000-0000-0000-000000000000, ou=users, o=smartdc"
                .to_string(),
            attrs,
            bin_attrs: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn extract_metadata_value_returns_none_when_attr_absent() {
        let entry = metadata_entry("other_key", "irrelevant");
        let value = extract_metadata_value(&entry, "usemoresecurity").unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn extract_metadata_value_parses_piranha_payload() {
        let entry = metadata_entry(
            "usemoresecurity",
            r#"{"secretkey":"GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"}"#,
        );
        let value = extract_metadata_value(&entry, "usemoresecurity")
            .unwrap()
            .expect("value present");
        assert_eq!(value["secretkey"], "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
    }

    #[test]
    fn extract_metadata_value_internal_error_on_non_json() {
        let entry = metadata_entry("usemoresecurity", "not json at all");
        let err = extract_metadata_value(&entry, "usemoresecurity")
            .expect_err("non-JSON value must surface as Err");
        assert!(matches!(err, SessionError::Internal(_)), "got {err:?}");
    }

    #[test]
    fn extract_secretkey_handles_disable_path() {
        // Piranha clears the secret on disable rather than removing
        // the metadata entry; an empty secretkey should read as
        // "not enrolled" rather than a malformed-secret error.
        let cleared = serde_json::json!({ "secretkey": "" });
        assert!(extract_secretkey(&cleared).is_none());

        let missing = serde_json::json!({ "other": "field" });
        assert!(extract_secretkey(&missing).is_none());

        let null = serde_json::json!({ "secretkey": null });
        assert!(extract_secretkey(&null).is_none());

        let valid = serde_json::json!({ "secretkey": "GEZDGNBVGY3TQOJQ" });
        assert_eq!(
            extract_secretkey(&valid).as_deref(),
            Some("GEZDGNBVGY3TQOJQ")
        );
    }
}
