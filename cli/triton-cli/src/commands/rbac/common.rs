// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Common utilities for RBAC commands

use anyhow::Result;
use cloudapi_client::TypedClient;

/// Resolve a user login name or UUID to a UUID string
pub async fn resolve_user(id_or_login: &str, client: &TypedClient) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_login).is_ok() {
        return Ok(id_or_login.to_string());
    }

    let account = &client.auth_config().account;
    let response = client.inner().list_users().account(account).send().await?;

    let users = response.into_inner();

    for user in &users {
        if user.login == id_or_login {
            return Ok(user.id.to_string());
        }
    }

    Err(anyhow::anyhow!("User not found: {}", id_or_login))
}
