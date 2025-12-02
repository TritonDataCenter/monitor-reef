// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

//! Stub JIRA server for testing
//!
//! This crate provides a Dropshot-based HTTP server that implements the JIRA API
//! trait with static test data. It can be used for:
//!
//! - Integration testing of bugview-service without a real JIRA instance
//! - Local development and demos
//! - End-to-end testing of the bugview CLI
//!
//! The server loads fixture data from JSON files at startup and serves it
//! via the standard JIRA REST API endpoints.

use anyhow::{Context, Result};
use dropshot::{HttpError, HttpResponseOk, Path, Query, RequestContext};
use jira_api::{Issue, IssueIdOrKey, IssueQuery, RemoteLink, SearchQuery, SearchResponse};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

// ============================================================================
// Fixture Data Types
// ============================================================================

/// Raw issue data as stored in fixtures (includes renderedFields at top level)
#[derive(Debug, Clone, Deserialize)]
struct FixtureIssue {
    key: String,
    id: String,
    fields: HashMap<String, serde_json::Value>,
    #[serde(default, rename = "renderedFields")]
    rendered_fields: Option<HashMap<String, serde_json::Value>>,
}

impl From<FixtureIssue> for Issue {
    fn from(f: FixtureIssue) -> Self {
        Issue {
            key: f.key,
            id: f.id,
            fields: f.fields,
            rendered_fields: f.rendered_fields,
        }
    }
}

// ============================================================================
// Server Context
// ============================================================================

/// Context for the stub JIRA server containing all test data
#[derive(Debug)]
pub struct StubContext {
    /// Issues indexed by key (e.g., "OS-8627")
    issues: HashMap<String, FixtureIssue>,
    /// Remote links indexed by issue key
    remote_links: HashMap<String, Vec<RemoteLink>>,
}

impl StubContext {
    /// Create a new stub context by loading fixture data from JSON files
    pub fn from_fixtures(fixtures_dir: &std::path::Path) -> Result<Self> {
        let issues_path = fixtures_dir.join("issues.json");
        let links_path = fixtures_dir.join("remote_links.json");

        let issues_json = std::fs::read_to_string(&issues_path)
            .with_context(|| format!("Failed to read {}", issues_path.display()))?;
        let issues: HashMap<String, FixtureIssue> = serde_json::from_str(&issues_json)
            .with_context(|| format!("Failed to parse {}", issues_path.display()))?;

        let remote_links: HashMap<String, Vec<RemoteLink>> = if links_path.exists() {
            let links_json = std::fs::read_to_string(&links_path)
                .with_context(|| format!("Failed to read {}", links_path.display()))?;
            serde_json::from_str(&links_json)
                .with_context(|| format!("Failed to parse {}", links_path.display()))?
        } else {
            HashMap::new()
        };

        Ok(Self {
            issues,
            remote_links,
        })
    }

    /// Get all issue keys
    pub fn issue_keys(&self) -> Vec<&str> {
        self.issues.keys().map(|s| s.as_str()).collect()
    }
}

// ============================================================================
// API Implementation
// ============================================================================

/// Marker type for the stub JIRA API implementation
pub enum StubJiraApi {}

impl jira_api::JiraApi for StubJiraApi {
    type Context = Arc<StubContext>;

    async fn search_issues(
        rqctx: RequestContext<Self::Context>,
        query: Query<SearchQuery>,
    ) -> Result<HttpResponseOk<SearchResponse>, HttpError> {
        let ctx = rqctx.context();
        let query = query.into_inner();

        // Parse the JQL to extract labels
        // Expected format: "labels IN (label1, label2) ORDER BY ..."
        let labels = parse_jql_labels(&query.jql);

        // Filter issues by labels
        let mut matching_issues: Vec<Issue> = ctx
            .issues
            .values()
            .filter(|issue| {
                if labels.is_empty() {
                    return true;
                }
                let issue_labels = issue
                    .fields
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();

                labels.iter().any(|l| issue_labels.contains(&l.as_str()))
            })
            .cloned()
            .map(Issue::from)
            .collect();

        // Sort by updated date descending (default JIRA behavior)
        matching_issues.sort_by(|a, b| {
            let a_updated = a
                .fields
                .get("updated")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let b_updated = b
                .fields
                .get("updated")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            b_updated.cmp(a_updated)
        });

        // Apply pagination
        let max_results = query.max_results.unwrap_or(50) as usize;
        let start = 0; // Simple implementation - no cursor support yet
        let end = std::cmp::min(start + max_results, matching_issues.len());
        let page_issues = matching_issues[start..end].to_vec();
        let is_last = end >= matching_issues.len();

        Ok(HttpResponseOk(SearchResponse {
            issues: page_issues,
            is_last: Some(is_last),
            next_page_token: if is_last {
                None
            } else {
                Some("stub_token".to_string())
            },
        }))
    }

    async fn get_issue(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssueIdOrKey>,
        query: Query<IssueQuery>,
    ) -> Result<HttpResponseOk<Issue>, HttpError> {
        let ctx = rqctx.context();
        let path = path.into_inner();
        let query = query.into_inner();

        // Look up by key or id
        let issue = ctx
            .issues
            .get(&path.issue_id_or_key)
            .or_else(|| ctx.issues.values().find(|i| i.id == path.issue_id_or_key))
            .ok_or_else(|| {
                HttpError::for_not_found(None, format!("Issue not found: {}", path.issue_id_or_key))
            })?;

        let mut result: Issue = issue.clone().into();

        // Only include renderedFields if expand=renderedFields is requested
        if let Some(expand) = &query.expand {
            if !expand.contains("renderedFields") {
                result.rendered_fields = None;
            }
        } else {
            result.rendered_fields = None;
        }

        Ok(HttpResponseOk(result))
    }

    async fn get_remote_links(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssueIdOrKey>,
    ) -> Result<HttpResponseOk<Vec<RemoteLink>>, HttpError> {
        let ctx = rqctx.context();
        let path = path.into_inner();

        // First verify the issue exists
        if !ctx.issues.contains_key(&path.issue_id_or_key)
            && !ctx.issues.values().any(|i| i.id == path.issue_id_or_key)
        {
            return Err(HttpError::for_not_found(
                None,
                format!("Issue not found: {}", path.issue_id_or_key),
            ));
        }

        let links = ctx
            .remote_links
            .get(&path.issue_id_or_key)
            .cloned()
            .unwrap_or_default();

        Ok(HttpResponseOk(links))
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Parse labels from a JQL query string
/// Expected format: "labels IN (label1, label2) ..."
fn parse_jql_labels(jql: &str) -> Vec<String> {
    // Simple regex-free parser for "labels IN (a, b, c)"
    let lower = jql.to_lowercase();
    if let Some(start) = lower.find("labels in (") {
        let after_paren = &jql[start + 11..];
        if let Some(end) = after_paren.find(')') {
            let labels_str = &after_paren[..end];
            return labels_str
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    Vec::new()
}

/// Create the Dropshot API description for the stub server
pub fn api_description() -> Result<dropshot::ApiDescription<Arc<StubContext>>, String> {
    jira_api::jira_api_mod::api_description::<StubJiraApi>().map_err(|e| e.to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jql_labels_simple() {
        let jql = "labels IN (public) ORDER BY updated DESC";
        let labels = parse_jql_labels(jql);
        assert_eq!(labels, vec!["public"]);
    }

    #[test]
    fn test_parse_jql_labels_multiple() {
        let jql = "labels IN (public, bug, feature) ORDER BY updated DESC";
        let labels = parse_jql_labels(jql);
        assert_eq!(labels, vec!["public", "bug", "feature"]);
    }

    #[test]
    fn test_parse_jql_labels_quoted() {
        let jql = r#"labels IN ("public", 'bug') ORDER BY updated DESC"#;
        let labels = parse_jql_labels(jql);
        assert_eq!(labels, vec!["public", "bug"]);
    }

    #[test]
    fn test_parse_jql_labels_none() {
        let jql = "project = OS ORDER BY updated DESC";
        let labels = parse_jql_labels(jql);
        assert!(labels.is_empty());
    }

    #[test]
    fn test_load_fixtures() {
        let fixtures_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let ctx = StubContext::from_fixtures(&fixtures_dir).expect("Failed to load fixtures");

        assert!(ctx.issues.contains_key("OS-8627"));
        assert!(ctx.issues.contains_key("TRITON-1813"));
        assert!(ctx.issues.contains_key("TOOLS-2590"));

        // Check that remote links were loaded
        assert!(ctx.remote_links.contains_key("OS-8627"));
        assert_eq!(ctx.remote_links.get("OS-8627").unwrap().len(), 2);
    }
}
