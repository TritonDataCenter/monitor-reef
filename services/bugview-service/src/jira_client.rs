//! JIRA REST API client wrapper
//!
//! This module provides a wrapper around the Progenitor-generated JIRA client
//! to maintain a clean interface for the bugview service.

use anyhow::{Context, Result};

// Re-export types from the generated client that match our API
pub use jira_client::types::{Issue, RemoteLink, SearchResponse};

/// JIRA client wrapper
#[derive(Clone)]
pub struct JiraClient {
    client: jira_client::Client,
}

impl JiraClient {
    /// Create a new JIRA client
    pub fn new(base_url: String, username: String, password: String) -> Result<Self> {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;
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

    /// Search for issues using JQL
    ///
    /// # Arguments
    /// * `labels` - Labels to filter by (combined with AND)
    /// * `page_token` - Optional pagination token (None for first page)
    /// * `sort` - Sort field (key, created, or updated)
    pub async fn search_issues(
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

        // Build the request
        let mut request = self.client.search_issues().jql(jql);

        request = request.max_results(max_results);
        request = request.fields("summary,resolution,updated,created".to_string());

        if let Some(token) = page_token {
            request = request.next_page_token(token.to_string());
        }

        let response = request
            .send()
            .await
            .context("Failed to send search request")?
            .into_inner();

        Ok(response)
    }

    /// Get a single issue by key
    ///
    /// # Arguments
    /// * `key` - Issue key (e.g., "PROJECT-123")
    pub async fn get_issue(&self, key: &str) -> Result<Issue> {
        if !key.contains('-') {
            anyhow::bail!("Invalid issue key: {}", key);
        }

        let response = self
            .client
            .get_issue()
            .key(key)
            .expand("renderedFields".to_string())
            .send()
            .await
            .map_err(|e| {
                let err_str = e.to_string();
                // Check for various 404 indicators
                // "Invalid Response Payload" means Progenitor couldn't deserialize JIRA's error response
                // which typically happens when JIRA returns 404 with their error JSON format
                if err_str.contains("404")
                    || err_str.contains("Not Found")
                    || err_str.contains("Invalid Response Payload")
                {
                    anyhow::anyhow!("Issue not found: {}", key)
                } else {
                    anyhow::anyhow!("Failed to get issue: {}", e)
                }
            })?;

        Ok(response.into_inner())
    }

    /// Get remote links for an issue
    ///
    /// # Arguments
    /// * `issue_id` - Issue ID (numeric, not the key)
    pub async fn get_remote_links(&self, issue_id: &str) -> Result<Vec<RemoteLink>> {
        if issue_id.contains('-') {
            anyhow::bail!("Issue ID must be numeric, not a key: {}", issue_id);
        }

        let response = self
            .client
            .get_remote_links()
            .key(issue_id)
            .send()
            .await
            .context("Failed to send get remote links request")?
            .into_inner();

        Ok(response)
    }
}
