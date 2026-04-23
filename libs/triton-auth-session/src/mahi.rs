// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Account/role lookup against the Mahi auth cache.
//!
//! Mahi mirrors UFDS accounts, users, roles, and — relevant here — the
//! `groupofuniquenames` memberships that decide operator/admin status.
//! [`MahiService::lookup`] just forwards to `GET /accounts?login=` and
//! returns mahi-client's own [`AuthInfo`] so callers can pick out whichever
//! fields they need without going through an intermediate type.
//!
//! Callers provide a prebuilt [`reqwest::Client`] at construction time —
//! typically via `triton_tls::build_http_client` — so this module stays
//! out of the TLS / CA-store business.

use crate::error::{SessionError, SessionResult};
use mahi_client::Client as MahiHttpClient;
pub use mahi_client::types::{AuthInfo, User};
use std::sync::Arc;
use tracing::warn;

pub struct MahiService {
    client: Arc<MahiHttpClient>,
}

impl MahiService {
    pub fn new(base_url: &str, http: reqwest::Client) -> Self {
        Self {
            client: Arc::new(MahiHttpClient::new_with_client(base_url, http)),
        }
    }

    /// Fetch the account for `login` and return mahi's [`AuthInfo`] verbatim.
    ///
    /// A 404 from mahi is treated as `AuthenticationFailed` — tritonapi's
    /// login handler has already verified a password at this point, so the
    /// only way the account can be missing is mahi-vs-UFDS replication lag
    /// for a just-created user. Reporting that as 401 lets the client retry
    /// cleanly; surfacing mahi's 404 would leak cache internals.
    pub async fn lookup(&self, login: &str) -> SessionResult<AuthInfo> {
        let resp = self
            .client
            .get_account()
            .login(login)
            .send()
            .await
            .map_err(|e| match e.status().map(|s| s.as_u16()) {
                Some(404) => SessionError::AuthenticationFailed,
                _ => {
                    warn!("mahi GET /accounts?login={login} failed: {e}");
                    SessionError::MahiUnavailable(format!("mahi lookup: {e}"))
                }
            })?;
        Ok(resp.into_inner())
    }

    /// Fetch the sub-user `user_login` under `account_login` and return
    /// mahi's [`AuthInfo`] with both `account` and `user` populated.
    ///
    /// `fallback=false` is passed so mahi 404s the request when the
    /// sub-user doesn't exist instead of silently returning an
    /// account-only record — that distinction matters for the SSH login
    /// path, where a missing sub-user must surface as
    /// `AuthenticationFailed` and not as "login succeeded with unexpected
    /// claims". 404s map to `AuthenticationFailed` for the same
    /// don't-enumerate-the-directory reason as [`Self::lookup`].
    pub async fn lookup_user(
        &self,
        account_login: &str,
        user_login: &str,
    ) -> SessionResult<AuthInfo> {
        let resp = self
            .client
            .get_user()
            .account(account_login)
            .login(user_login)
            .fallback(false)
            .send()
            .await
            .map_err(|e| match e.status().map(|s| s.as_u16()) {
                Some(404) => SessionError::AuthenticationFailed,
                _ => {
                    warn!("mahi GET /users?account={account_login}&login={user_login} failed: {e}");
                    SessionError::MahiUnavailable(format!("mahi sub-user lookup: {e}"))
                }
            })?;
        Ok(resp.into_inner())
    }
}
