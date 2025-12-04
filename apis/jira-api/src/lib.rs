// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

//! JIRA API Trait Definition
//!
//! **IMPORTANT**: This trait defines a *subset* of the JIRA REST API v3.
//! This is NOT a complete JIRA API definition - it only includes the specific
//! endpoints used by the bugview service for querying public issues.
//!
//! The actual JIRA API is implemented by Atlassian's JIRA servers. This trait
//! exists to:
//! 1. Document the exact JIRA API surface we depend on
//! 2. Generate an OpenAPI specification for client generation
//! 3. Enable mock implementations for testing
//! 4. Serve as a real-world example of API trait definitions in this monorepo
//!
//! Reference: https://developer.atlassian.com/cloud/jira/platform/rest/v3/

use dropshot::{HttpError, HttpResponseOk, Path, Query, RequestContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

// ============================================================================
// Newtypes
// ============================================================================

/// A JIRA issue key in PROJECT-123 format
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct IssueKey(String);

impl IssueKey {
    /// Create a new IssueKey, validating the format
    pub fn new(key: impl Into<String>) -> Result<Self, InvalidIssueKey> {
        let key = key.into();
        // Must contain a hyphen and have at least one digit after
        if key.contains('-')
            && key
                .split('-')
                .last()
                .map_or(false, |n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
        {
            Ok(Self(key))
        } else {
            Err(InvalidIssueKey(key))
        }
    }

    /// Create without validation (for trusted sources like JIRA responses)
    pub fn new_unchecked(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IssueKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for IssueKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub struct InvalidIssueKey(pub String);

impl fmt::Display for InvalidIssueKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid issue key format: '{}' (expected PROJECT-123)",
            self.0
        )
    }
}

impl std::error::Error for InvalidIssueKey {}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Query parameters for issue search endpoint
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchQuery {
    /// JQL (JIRA Query Language) query string
    pub jql: String,

    /// Maximum number of results to return (default: 50)
    #[serde(rename = "maxResults")]
    pub max_results: Option<u32>,

    /// Comma-separated list of fields to include in the response
    pub fields: Option<String>,

    /// Token for cursor-based pagination (returned from previous search)
    #[serde(rename = "nextPageToken")]
    pub next_page_token: Option<String>,
}

/// Response from JIRA search endpoint
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    /// List of issues matching the query
    pub issues: Vec<Issue>,

    /// Whether this is the last page of results
    #[serde(rename = "isLast", default)]
    pub is_last: Option<bool>,

    /// Token for fetching the next page of results (cursor-based pagination)
    #[serde(rename = "nextPageToken", default)]
    pub next_page_token: Option<String>,
}

// NOTE: In the official Atlassian JIRA openapi spec,
// this data structure is called "IssueBean".
/// Full issue details
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Issue {
    /// Issue key (e.g., "PROJECT-123")
    pub key: IssueKey,

    /// Issue ID (numeric)
    pub id: String,

    /// Issue fields as a dynamic JSON object
    pub fields: HashMap<String, serde_json::Value>,

    /// Rendered (HTML) versions of fields when expand=renderedFields is used
    #[serde(default, rename = "renderedFields")]
    pub rendered_fields: Option<HashMap<String, serde_json::Value>>,
}

/// Path parameter for get_issue endpoint
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IssueIdOrKey {
    /// Issue ID (some opaque number) or Key (e.g., "PROJECT-123")
    pub issue_id_or_key: String,
}

/// Query parameters for get_issue endpoint
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IssueQuery {
    /// Comma-separated list of expansions (e.g., "renderedFields")
    pub expand: Option<String>,
}

/// Remote link information
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoteLink {
    /// Remote link ID
    pub id: u64,

    /// Remote link object with URL and title
    pub object: Option<RemoteLinkObject>,
}

/// Remote link object details
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoteLinkObject {
    /// URL of the remote resource
    pub url: String,

    /// Title/description of the remote resource
    pub title: String,
}

// ============================================================================
// API Trait
// ============================================================================

/// JIRA REST API v3 (Subset)
///
/// **IMPORTANT**: This is a partial definition of JIRA's API, containing only
/// the endpoints used by bugview-service. This is NOT a complete JIRA client.
///
/// The actual implementation of these endpoints is provided by Atlassian's JIRA
/// servers, not by us. We define this trait to generate a client via Progenitor.
#[dropshot::api_description]
pub trait JiraApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    /// Search for issues using JQL
    ///
    /// Searches for issues using JIRA Query Language (JQL). Supports cursor-based
    /// pagination via the nextPageToken parameter.
    ///
    /// **JIRA API Reference**: GET /rest/api/3/search/jql
    #[endpoint {
        method = GET,
        path = "/rest/api/3/search/jql",
        tags = ["issue-search"],
    }]
    async fn search_issues(
        rqctx: RequestContext<Self::Context>,
        query: Query<SearchQuery>,
    ) -> Result<HttpResponseOk<SearchResponse>, HttpError>;

    /// Get a single issue by key
    ///
    /// Retrieves full details for a specific issue. Use the expand parameter
    /// to request additional data like renderedFields (HTML-rendered field values).
    ///
    /// **JIRA API Reference**: GET /rest/api/3/issue/{issueIdOrKey}
    #[endpoint {
        method = GET,
        path = "/rest/api/3/issue/{issue_id_or_key}",
        tags = ["issues"],
    }]
    async fn get_issue(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssueIdOrKey>,
        query: Query<IssueQuery>,
    ) -> Result<HttpResponseOk<Issue>, HttpError>;

    /// Get remote links for an issue
    ///
    /// Retrieves all remote links associated with an issue. Remote links are
    /// external URLs related to the issue.
    ///
    /// **JIRA API Reference**: GET /rest/api/3/issue/{issueIdOrKey}/remotelink
    #[endpoint {
        method = GET,
        path = "/rest/api/3/issue/{issue_id_or_key}/remotelink",
        tags = ["issue-links"],
    }]
    async fn get_remote_links(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssueIdOrKey>,
    ) -> Result<HttpResponseOk<Vec<RemoteLink>>, HttpError>;
}
