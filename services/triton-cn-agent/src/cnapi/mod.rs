// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Minimal CNAPI HTTP client for cn-agent.
//!
//! cn-agent only touches a handful of CNAPI endpoints:
//!
//! * `POST /servers/{uuid}/events/heartbeat` — liveness ping (every ~5s)
//! * `POST /servers/{uuid}/events/status` — periodic status report
//! * `POST /servers/{uuid}/sysinfo` — register/refresh sysinfo
//! * `POST /servers/{uuid}` — update agents list
//!
//! Each one takes the compute-node UUID in the path and a JSON body. This
//! module wraps them with a single [`CnapiClient`] that owns a reqwest
//! client; the heartbeater drives the whole thing.

use std::time::Duration;

use reqwest::StatusCode;
use thiserror::Error;

use cn_agent_api::Uuid;

/// Default timeouts match the legacy restify client:
/// `connectTimeout: 5000, requestTimeout: 5000`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum CnapiError {
    #[error("failed to build reqwest client: {0}")]
    BuildClient(#[source] reqwest::Error),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("CNAPI returned status {status}: {body}")]
    Status { status: StatusCode, body: String },
    #[error("JSON encode failed: {0}")]
    Encode(#[from] serde_json::Error),
}

impl CnapiError {
    /// Whether the legacy agent interprets this as "CNAPI does not support
    /// sysinfo registration, don't retry" (a 404 with restCode
    /// `ResourceNotFound`).
    pub fn is_sysinfo_unsupported(&self) -> bool {
        matches!(self, CnapiError::Status { status, .. } if *status == StatusCode::NOT_FOUND)
    }
}

/// A single agent entry posted to `POST /servers/{uuid}`.
///
/// Matches the JSON shape the legacy agent sends (see
/// `lib/backends/smartos/index.js:getAgents`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub image_uuid: String,
    /// Optional instance UUID from `/opt/smartdc/agents/etc/<name>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Client for the compute-node-facing slice of CNAPI.
///
/// Holds the CN's server UUID so callers don't have to pass it every call.
#[derive(Debug, Clone)]
pub struct CnapiClient {
    base_url: String,
    server_uuid: Uuid,
    http: reqwest::Client,
}

impl CnapiClient {
    /// Build a client with the legacy default timeouts.
    pub fn new(base_url: impl Into<String>, server_uuid: Uuid) -> Result<Self, CnapiError> {
        Self::builder(base_url, server_uuid).build()
    }

    pub fn builder(base_url: impl Into<String>, server_uuid: Uuid) -> CnapiClientBuilder {
        CnapiClientBuilder::new(base_url, server_uuid)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn server_uuid(&self) -> Uuid {
        self.server_uuid
    }

    /// `POST /servers/{uuid}/events/heartbeat`.
    ///
    /// Sends an empty JSON object `{}`, not an empty body — the legacy
    /// agent does the same (`self.cnapiClient.post(path, {}, ...)`) and
    /// Dropshot's `TypedBody<Value>` rejects a truly empty body with 400.
    pub async fn post_heartbeat(&self) -> Result<(), CnapiError> {
        let path = format!("/servers/{}/events/heartbeat", self.server_uuid);
        self.post_json(&path, &serde_json::json!({})).await
    }

    /// `POST /servers/{uuid}/events/status` with the given status report.
    pub async fn post_status(&self, status: &serde_json::Value) -> Result<(), CnapiError> {
        let path = format!("/servers/{}/events/status", self.server_uuid);
        self.post_json(&path, status).await
    }

    /// `POST /servers/{uuid}/sysinfo` with `{sysinfo: <sysinfo>}`.
    ///
    /// cn-agent injects `CN Agent Port` into sysinfo before posting so CNAPI
    /// knows how to dial us back; the caller is responsible for that.
    pub async fn register_sysinfo(&self, sysinfo: &serde_json::Value) -> Result<(), CnapiError> {
        let path = format!("/servers/{}/sysinfo", self.server_uuid);
        let body = serde_json::json!({ "sysinfo": sysinfo });
        self.post_json(&path, &body).await
    }

    /// `POST /servers/{uuid}` with `{agents: [...]}`.
    pub async fn post_agents(&self, agents: &[AgentInfo]) -> Result<(), CnapiError> {
        let path = format!("/servers/{}", self.server_uuid);
        let body = serde_json::json!({ "agents": agents });
        self.post_json(&path, &body).await
    }

    async fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<(), CnapiError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.http.post(&url).json(body).send().await?;
        Self::ensure_success(resp).await
    }

    async fn ensure_success(resp: reqwest::Response) -> Result<(), CnapiError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(CnapiError::Status { status, body })
    }
}

/// Builder for [`CnapiClient`].
pub struct CnapiClientBuilder {
    base_url: String,
    server_uuid: Uuid,
    connect_timeout: Duration,
    request_timeout: Duration,
    user_agent: Option<String>,
}

impl CnapiClientBuilder {
    pub fn new(base_url: impl Into<String>, server_uuid: Uuid) -> Self {
        Self {
            base_url: base_url.into(),
            server_uuid,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            user_agent: None,
        }
    }

    pub fn with_connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    pub fn with_request_timeout(mut self, d: Duration) -> Self {
        self.request_timeout = d;
        self
    }

    pub fn with_user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    pub fn build(self) -> Result<CnapiClient, CnapiError> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.request_timeout);
        if let Some(ua) = self.user_agent {
            builder = builder.user_agent(ua);
        }
        let http = builder.build().map_err(CnapiError::BuildClient)?;
        Ok(CnapiClient {
            base_url: self.base_url.trim_end_matches('/').to_string(),
            server_uuid: self.server_uuid,
            http,
        })
    }
}
