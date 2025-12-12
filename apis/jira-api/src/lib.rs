// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

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
///
/// This type validates the format on deserialization to prevent invalid keys
/// from being silently accepted. Valid keys must have the format `PROJECT-123`
/// where PROJECT is one or more characters and 123 is one or more ASCII digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct IssueKey(String);

impl<'de> Deserialize<'de> for IssueKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        IssueKey::new(s).map_err(serde::de::Error::custom)
    }
}

impl IssueKey {
    /// Create a new IssueKey, validating the format
    ///
    /// Valid format: `PROJECT-123` where:
    /// - PROJECT is one or more characters (the project prefix)
    /// - 123 is one or more ASCII digits (the issue number)
    pub fn new(key: impl Into<String>) -> Result<Self, InvalidIssueKey> {
        let key = key.into();
        // Find the last hyphen to split prefix and number
        if let Some(hyphen_pos) = key.rfind('-') {
            let prefix = &key[..hyphen_pos];
            let number = &key[hyphen_pos + 1..];
            // Require non-empty prefix and non-empty numeric suffix
            if !prefix.is_empty()
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
            {
                return Ok(Self(key));
            }
        }
        Err(InvalidIssueKey(key))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_key_valid_formats() {
        // Standard formats
        assert!(IssueKey::new("PROJECT-123").is_ok());
        assert!(IssueKey::new("A-1").is_ok());
        assert!(IssueKey::new("PROJ-99999").is_ok());
        assert!(IssueKey::new("TRITON-1813").is_ok());
        assert!(IssueKey::new("OS-6892").is_ok());

        // Multiple hyphens in project name are valid
        assert!(IssueKey::new("MY-PROJECT-123").is_ok());
    }

    #[test]
    fn issue_key_invalid_formats() {
        // No hyphen
        assert!(IssueKey::new("PROJECT123").is_err());
        assert!(IssueKey::new("123").is_err());

        // No digits after hyphen
        assert!(IssueKey::new("PROJECT-").is_err());
        assert!(IssueKey::new("PROJECT-abc").is_err());
        assert!(IssueKey::new("-123").is_err());

        // Empty string
        assert!(IssueKey::new("").is_err());

        // Just a hyphen
        assert!(IssueKey::new("-").is_err());

        // Mixed letters and digits after hyphen
        assert!(IssueKey::new("PROJECT-12abc").is_err());
        assert!(IssueKey::new("PROJECT-abc12").is_err());
    }

    #[test]
    fn issue_key_deserialization_validates() {
        // Valid key deserializes successfully
        let valid: Result<IssueKey, _> = serde_json::from_str(r#""PROJECT-123""#);
        assert!(valid.is_ok());
        assert_eq!(valid.unwrap().as_str(), "PROJECT-123");

        // Invalid key fails deserialization
        let invalid: Result<IssueKey, _> = serde_json::from_str(r#""invalid""#);
        assert!(invalid.is_err());

        let invalid_no_digits: Result<IssueKey, _> = serde_json::from_str(r#""PROJECT-abc""#);
        assert!(invalid_no_digits.is_err());
    }

    #[test]
    fn issue_key_serialization() {
        let key = IssueKey::new("PROJECT-123").unwrap();
        let serialized = serde_json::to_string(&key).unwrap();
        assert_eq!(serialized, r#""PROJECT-123""#);
    }

    #[test]
    fn issue_key_display() {
        let key = IssueKey::new("PROJECT-123").unwrap();
        assert_eq!(format!("{}", key), "PROJECT-123");
    }

    #[test]
    fn invalid_issue_key_error_message() {
        let err = IssueKey::new("invalid").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid"));
        assert!(msg.contains("PROJECT-123"));
    }
}
