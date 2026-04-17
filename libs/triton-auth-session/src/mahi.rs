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

use crate::error::{SessionError, SessionResult};
use mahi_client::Client as MahiHttpClient;
pub use mahi_client::types::AuthInfo;
use std::sync::Arc;
use tracing::warn;
use url::Url;

#[derive(Debug, Clone)]
pub struct MahiConfig {
    pub url: Url,
}

pub struct MahiService {
    client: Arc<MahiHttpClient>,
}

impl MahiService {
    pub fn new(config: MahiConfig) -> Self {
        Self {
            client: Arc::new(MahiHttpClient::new(config.url.as_str())),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_constructs() {
        let _svc = MahiService::new(MahiConfig {
            url: Url::parse("http://mahi.example.invalid").unwrap(),
        });
    }
}
