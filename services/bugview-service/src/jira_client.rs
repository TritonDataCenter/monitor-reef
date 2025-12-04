// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! JIRA REST API client wrapper
//!
//! This module provides a wrapper around the Progenitor-generated JIRA client
//! to maintain a clean interface for the bugview service.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::time::Duration;

// Retry/backoff and HTTP configuration
const RETRY_MAX_RETRIES: u32 = 3;
const RETRY_INITIAL_DELAY_MS: u64 = 150;
const RETRY_MAX_DELAY_MS: u64 = 2_000;
const RETRY_JITTER_MAX_MS: u64 = 50;
const HTTP_TIMEOUT_SECS: u64 = 15;
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
const JIRA_SEARCH_MAX_RESULTS: u32 = 50;

// Re-export types from jira-api for consistency
pub use jira_api::{Issue, IssueKey, RemoteLink};

// Custom SearchResponse that uses jira_api::Issue instead of generated Issue
#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub issues: Vec<Issue>,
    pub is_last: Option<bool>,
    pub next_page_token: Option<String>,
}

impl From<jira_client::types::SearchResponse> for SearchResponse {
    fn from(resp: jira_client::types::SearchResponse) -> Self {
        Self {
            issues: resp.issues.into_iter().map(Into::into).collect(),
            is_last: resp.is_last,
            next_page_token: resp.next_page_token,
        }
    }
}

/// Trait abstraction for the JIRA client used by the service.
#[async_trait]
pub trait JiraClientTrait: Send + Sync {
    async fn search_issues(
        &self,
        labels: &[String],
        page_token: Option<&str>,
        sort: &str,
    ) -> Result<SearchResponse>;

    async fn get_issue(&self, key: &IssueKey) -> Result<Issue>;

    async fn get_remote_links(&self, issue_id: &str) -> Result<Vec<RemoteLink>>;
}

/// Concrete JIRA client wrapper backed by the Progenitor-generated client.
#[derive(Clone)]
pub struct JiraClient {
    client: jira_client::Client,
}

impl JiraClient {
    /// Create a new JIRA client
    pub fn new(base_url: String, username: String, password: String) -> Result<Self> {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;
        use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

        // Create Basic Auth header
        let mut headers = HeaderMap::new();
        let credentials = format!("{}:{}", username, password);
        let encoded = STANDARD.encode(credentials.as_bytes());
        let auth_value = format!("Basic {}", encoded);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_value).context("Failed to create auth header")?,
        );

        // Create authenticated HTTP client
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .build()
            .context("Failed to create HTTP client")?;

        let client = jira_client::Client::new_with_client(&base_url, http_client);

        Ok(Self { client })
    }
}

async fn with_retries<F, Fut, T>(mut f: F, op_name: &str) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let max_retries = RETRY_MAX_RETRIES;
    let mut attempt = 0u32;
    let mut delay = Duration::from_millis(RETRY_INITIAL_DELAY_MS);

    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                attempt += 1;
                let es = e.to_string();
                let retriable = es.contains("429")
                    || es.to_lowercase().contains("too many requests")
                    || es.to_lowercase().contains("timeout")
                    || es.to_lowercase().contains("timed out")
                    || es.to_lowercase().contains("connection")
                    || es.to_lowercase().contains("temporarily unavailable")
                    || es.contains("5xx")
                    || es.contains("500");

                if !retriable || attempt > max_retries {
                    return Err(anyhow::anyhow!(
                        "{} failed after {} attempt(s): {}",
                        op_name,
                        attempt,
                        es
                    ));
                }

                // Exponential backoff with jitter (uniform)
                let jitter: u64 = rand::random_range(0..RETRY_JITTER_MAX_MS);
                tokio::time::sleep(delay + Duration::from_millis(jitter)).await;
                delay = std::cmp::min(delay * 2, Duration::from_millis(RETRY_MAX_DELAY_MS));
            }
        }
    }
}

#[async_trait]
impl JiraClientTrait for JiraClient {
    async fn search_issues(
        &self,
        labels: &[String],
        page_token: Option<&str>,
        sort: &str,
    ) -> Result<SearchResponse> {
        let max_results = JIRA_SEARCH_MAX_RESULTS;

        // Build JQL query
        let label_clauses: Vec<String> = labels
            .iter()
            .map(|label| format!("labels in (\"{}\")", label))
            .collect();
        let mut jql = label_clauses.join(" AND ");

        // Add sort clause
        if sort == "created" || sort == "updated" {
            if !jql.is_empty() {
                jql.push(' ');
            }
            jql.push_str(&format!("ORDER BY {} DESC", sort));
        }

        // Capture values for request reconstruction per-attempt
        let jql_owned = jql;
        let token_owned = page_token.map(|s| s.to_string());

        with_retries(
            || async {
                let mut request = self.client.search_issues().jql(jql_owned.clone());
                request = request.max_results(max_results);
                request = request.fields("summary,status,resolution,updated,created".to_string());
                if let Some(ref token) = token_owned {
                    request = request.next_page_token(token.clone());
                }
                let response = request
                    .send()
                    .await
                    .context("Failed to send search request")?
                    .into_inner();

                // Convert SearchResponse to use jira_api::Issue
                Ok(response.into())
            },
            "jira.search_issues",
        )
        .await
    }

    async fn get_issue(&self, key: &IssueKey) -> Result<Issue> {
        let key_str = key.as_str();

        with_retries(
            || async {
                let k = key_str.to_string();
                let response = self
                    .client
                    .get_issue()
                    .issue_id_or_key(&k)
                    .send()
                    .await
                    .map_err(|e| {
                        let err_str = e.to_string();
                        if err_str.contains("404")
                            || err_str.contains("Not Found")
                            || err_str.contains("Invalid Response Payload")
                        {
                            anyhow::anyhow!("Issue not found: {}", k)
                        } else if err_str.contains("429")
                            || err_str.to_lowercase().contains("too many requests")
                        {
                            anyhow::anyhow!("Rate limited by JIRA (429) when getting issue {}", k)
                        } else {
                            anyhow::anyhow!("Failed to get issue: {}", e)
                        }
                    })?;
                // Convert from generated Issue (String key) to jira_api::Issue (IssueKey)
                Ok(response.into_inner().into())
            },
            "jira.get_issue",
        )
        .await
    }

    async fn get_remote_links(&self, issue_id: &str) -> Result<Vec<RemoteLink>> {
        if issue_id.contains('-') {
            anyhow::bail!("Issue ID must be numeric, not a key: {}", issue_id);
        }

        with_retries(
            || async {
                let key_id = issue_id.to_string();
                let response = self
                    .client
                    .get_remote_links()
                    .issue_id_or_key(&key_id)
                    .send()
                    .await
                    .context("Failed to send get remote links request")?
                    .into_inner();
                // Convert from generated RemoteLink to jira_api::RemoteLink
                Ok(response.into_iter().map(Into::into).collect())
            },
            "jira.get_remote_links",
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_with_retries_succeeds_after_errors() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempts = AtomicU32::new(0);
        let res: Result<u32> = with_retries(
            || {
                let n = attempts.fetch_add(1, Ordering::Relaxed) + 1;
                async move {
                    if n < 3 {
                        Err(anyhow::anyhow!("temporary connection error"))
                    } else {
                        Ok(42)
                    }
                }
            },
            "test.op",
        )
        .await;

        assert!(res.is_ok());
        assert_eq!(res.unwrap(), 42);
    }
}
