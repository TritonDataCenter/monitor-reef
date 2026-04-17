// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Minimal client for the `firewaller` CN agent (port 2021).
//!
//! machine_create accepts an optional `firewall_rules: [...]` array and
//! forwards each entry to the local firewaller via `PUT /rules/{uuid}`.
//! The firewaller runs on the same admin IP, so this client is just
//! `reqwest` wrapped to produce the right URL.

use std::net::Ipv4Addr;
use std::time::Duration;

use thiserror::Error;

/// TCP port firewaller listens on (always 2021 on a Triton CN).
pub const DEFAULT_FIREWALLER_PORT: u16 = 2021;

/// Connection + request timeout for each PUT. Firewaller is local, so
/// generous on network but stingy on wall time.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum FirewallerError {
    #[error("failed to build reqwest client: {0}")]
    BuildClient(#[source] reqwest::Error),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("firewaller returned status {status}: {body}")]
    Status {
        status: reqwest::StatusCode,
        body: String,
    },
}

/// A single firewall rule entry. We pass the whole JSON blob through
/// rather than typing every field, because firewaller itself defines
/// the schema and we don't want to constrain it at the cn-agent layer.
#[derive(Debug, Clone)]
pub struct FirewallRule {
    pub uuid: String,
    pub payload: serde_json::Value,
}

/// Admin-IP-scoped firewaller HTTP client.
#[derive(Debug, Clone)]
pub struct FirewallerClient {
    http: reqwest::Client,
    base_url: String,
}

impl FirewallerClient {
    /// Build a client that talks to `http://<admin_ip>:<port>`.
    pub fn new(admin_ip: Ipv4Addr, request_id: Option<&str>) -> Result<Self, FirewallerError> {
        Self::with_port(admin_ip, DEFAULT_FIREWALLER_PORT, request_id)
    }

    pub fn with_port(
        admin_ip: Ipv4Addr,
        port: u16,
        request_id: Option<&str>,
    ) -> Result<Self, FirewallerError> {
        let mut builder = reqwest::Client::builder().timeout(DEFAULT_TIMEOUT);
        if let Some(id) = request_id {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Ok(v) = reqwest::header::HeaderValue::from_str(id) {
                headers.insert("x-request-id", v);
            }
            builder = builder.default_headers(headers);
        }
        let http = builder.build().map_err(FirewallerError::BuildClient)?;
        Ok(Self {
            http,
            base_url: format!("http://{admin_ip}:{port}"),
        })
    }

    /// Override the base URL (used by integration tests).
    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self, FirewallerError> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(FirewallerError::BuildClient)?;
        Ok(Self {
            http,
            base_url: base_url.into(),
        })
    }

    /// `PUT /rules/{uuid}` with the rule body.
    pub async fn put_rule(&self, rule: &FirewallRule) -> Result<(), FirewallerError> {
        let url = format!("{}/rules/{}", self.base_url, rule.uuid);
        let resp = self.http.put(&url).json(&rule.payload).send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(FirewallerError::Status { status, body })
    }

    /// PUT every rule in sequence. Fails fast on the first error, but
    /// the caller decides whether that aborts the overall task.
    pub async fn put_rules(&self, rules: &[FirewallRule]) -> Result<(), FirewallerError> {
        for rule in rules {
            self.put_rule(rule).await?;
        }
        Ok(())
    }
}
