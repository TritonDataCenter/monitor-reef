// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Authentication support for CloudAPI requests
//!
//! This module provides the `add_auth_headers` pre-hook function for adding
//! HTTP Signature authentication headers to Progenitor-generated client requests.

use triton_auth::AuthConfig;

/// Add authentication headers to a request
///
/// This function is used as a `pre_hook_async` for the Progenitor-generated client.
/// It adds the required `Date` and `Authorization` headers for HTTP Signature auth.
///
/// # Arguments
/// * `auth_config` - Authentication configuration containing account, key_id, and key_source
/// * `request` - The mutable request to add headers to
///
/// # Errors
/// Returns an error if signing fails (key not found, agent unavailable, etc.)
pub async fn add_auth_headers(
    auth_config: &AuthConfig,
    request: &mut reqwest::Request,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let method = request.method().as_str();
    let path = request.url().path();

    // Sign the request using triton-auth
    let (date_header, auth_header) = triton_auth::sign_request(auth_config, method, path).await?;

    // Add headers
    let headers = request.headers_mut();
    headers.insert(
        reqwest::header::DATE,
        date_header.parse().map_err(|e| {
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid date header: {}", e),
            )) as Box<dyn std::error::Error + Send + Sync>
        })?,
    );
    headers.insert(
        reqwest::header::AUTHORIZATION,
        auth_header.parse().map_err(|e| {
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid authorization header: {}", e),
            )) as Box<dyn std::error::Error + Send + Sync>
        })?,
    );

    // Add RBAC roles as query parameter if present
    if let Some(roles) = &auth_config.roles
        && !roles.is_empty()
    {
        let url = request.url_mut();
        let mut query = url.query().unwrap_or("").to_string();
        if !query.is_empty() {
            query.push('&');
        }
        query.push_str(&format!("as-role={}", roles.join(",")));
        url.set_query(Some(&query));
    }

    Ok(())
}
