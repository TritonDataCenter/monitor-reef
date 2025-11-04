//! JIRA REST API client wrapper
//!
//! This module provides a wrapper around the Progenitor-generated JIRA client
//! to maintain a clean interface for the bugview service.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::time::Duration;

// Re-export types from the generated client that match our API
pub use jira_client::types::{Issue, RemoteLink, SearchResponse};

/// Trait abstraction for the JIRA client used by the service.
#[async_trait]
pub trait JiraClientTrait: Send + Sync {
    async fn search_issues(
        &self,
        labels: &[String],
        page_token: Option<&str>,
        sort: &str,
    ) -> Result<SearchResponse>;

    async fn get_issue(&self, key: &str) -> Result<Issue>;

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
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

        // Create Basic Auth header
        let mut headers = HeaderMap::new();
        let credentials = format!("{}:{}", username, password);
        let encoded = STANDARD.encode(credentials.as_bytes());
        let auth_value = format!("Basic {}", encoded);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_value)
                .context("Failed to create auth header")?,
        );

        // Create authenticated HTTP client
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("BugviewRust/0.1.0")
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
    let max_retries = 3u32;
    let mut attempt = 0u32;
    let mut delay = Duration::from_millis(150);

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
                        op_name, attempt, es
                    ));
                }

                // Exponential backoff with jitter
                let jitter: u64 = (rand::random::<u8>() as u64) % 50;
                tokio::time::sleep(delay + Duration::from_millis(jitter)).await;
                delay = std::cmp::min(delay * 2, Duration::from_secs(2));
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
        let max_results = 50;

        // Build JQL query
        let label_clauses: Vec<String> = labels
            .iter()
            .map(|label| format!("labels in (\"{}\")", label))
            .collect();
        let mut jql = label_clauses.join(" AND ");

        // Add sort clause
        if sort == "created" || sort == "updated" {
            if !jql.is_empty() {
                jql.push_str(" ");
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
                request = request.fields("summary,resolution,updated,created".to_string());
                if let Some(ref token) = token_owned {
                    request = request.next_page_token(token.clone());
                }
                let response = request
                    .send()
                    .await
                    .context("Failed to send search request")?
                    .into_inner();
                Ok(response)
            },
            "jira.search_issues",
        )
        .await
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        if !key.contains('-') {
            anyhow::bail!("Invalid issue key: {}", key);
        }

        with_retries(
            || async {
                let k = key.to_string();
                let response = self
                    .client
                    .get_issue()
                    .key(&k)
                    .expand("renderedFields".to_string())
                    .send()
                    .await
                    .map_err(|e| {
                        let err_str = e.to_string();
                        if err_str.contains("404")
                            || err_str.contains("Not Found")
                            || err_str.contains("Invalid Response Payload")
                        {
                            anyhow::anyhow!("Issue not found: {}", k)
                        } else if err_str.contains("429") || err_str.to_lowercase().contains("too many requests") {
                            anyhow::anyhow!("Rate limited by JIRA (429) when getting issue {}", k)
                        } else {
                            anyhow::anyhow!("Failed to get issue: {}", e)
                        }
                    })?;
                Ok(response.into_inner())
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
                    .key(&key_id)
                    .send()
                    .await
                    .context("Failed to send get remote links request")?
                    .into_inner();
                Ok(response)
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
        let mut attempts = 0u32;
        let res: Result<u32> = with_retries(
            || {
                attempts += 1;
                async move {
                    if attempts < 3 {
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
