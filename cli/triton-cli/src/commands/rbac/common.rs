// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common utilities for RBAC commands

use anyhow::Result;
use cloudapi_api::User;

use crate::client::AnyClient;
use crate::dispatch;

/// Resolve a user login name or UUID to a UUID string
pub async fn resolve_user(id_or_login: &str, client: &AnyClient) -> Result<String> {
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_login) {
        // NOTE: We accept the parsed ID without verifying it exists server-side, matching node-triton's behavior.
        return Ok(uuid.to_string());
    }

    let account = client.effective_account();
    let users: Vec<User> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_users()
            .account(account)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Vec<User>>(serde_json::to_value(&resp)?)?
    });

    for user in &users {
        if user.login == id_or_login {
            return Ok(user.id.to_string());
        }
    }

    Err(crate::errors::ResourceNotFoundError(format!("User not found: {}", id_or_login)).into())
}
