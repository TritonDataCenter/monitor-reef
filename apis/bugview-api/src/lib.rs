// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

use dropshot::{Body, HttpError, HttpResponseOk, Path, Query, RequestContext};
use http::Response;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Modules
// ============================================================================

pub mod adf;

// ============================================================================
// Request/Response Types
// ============================================================================

/// Sort field for issue lists
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IssueSort {
    /// Sort by issue key (e.g., OS-1234)
    Key,
    /// Sort by creation date
    Created,
    /// Sort by last update date (default)
    #[default]
    Updated,
}

impl IssueSort {
    /// Returns the sort field as a string for use in JQL queries
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueSort::Key => "key",
            IssueSort::Created => "created",
            IssueSort::Updated => "updated",
        }
    }
}

impl std::fmt::Display for IssueSort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Query parameters for issue list endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssueListQuery {
    /// Next page token for pagination (token-based, not offset)
    #[serde(default)]
    pub next_page_token: Option<String>,
    /// Sort field (key, created, or updated). Defaults to "updated" if omitted.
    #[serde(default)]
    pub sort: Option<IssueSort>,
}

/// Path parameter for label-specific queries
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LabelPath {
    /// Label key
    pub key: String,
}

/// Path parameter for issue-specific queries
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssuePath {
    /// Issue key (e.g., "PROJECT-123")
    pub key: String,
}

/// Simplified issue information for list views
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IssueListItem {
    /// Issue key (e.g., "PROJECT-123")
    pub key: jira_api::IssueKey,
    /// Issue summary/title
    pub summary: String,
    /// Issue status (e.g., "Open", "Resolved", "Closed")
    pub status: String,
    /// Resolution status (if resolved)
    pub resolution: Option<String>,
    /// Last updated timestamp
    pub updated: String,
    /// Creation timestamp
    pub created: String,
}

/// Legacy issue summary format (for backwards compatibility with Node.js bugview)
///
/// This matches the original `/bugview/json/{key}` response format.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IssueSummary {
    /// Issue key (named "id" for legacy compatibility)
    pub id: String,
    /// Issue summary/title
    pub summary: String,
    /// URL to the bugview web page for this issue
    pub web_url: String,
}

/// Response for issue list endpoints
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IssueListResponse {
    /// Issues in this page (up to 50)
    pub issues: Vec<IssueListItem>,
    /// Token for next page (None if this is the last page)
    pub next_page_token: Option<String>,
    /// True if this is the last page
    pub is_last: bool,
}

/// Full issue details (matches original Node.js bugview format)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IssueDetails {
    /// JIRA internal issue ID
    pub id: String,
    /// Issue key (e.g., "OS-1234")
    pub key: jira_api::IssueKey,
    /// Issue fields (sanitized for public consumption)
    pub fields: serde_json::Value,
    /// Remote links (filtered by allowed domains)
    pub remotelinks: Vec<RemoteLink>,
}

/// Remote link information
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoteLink {
    /// Link URL
    pub url: String,
    /// Link title/description
    pub title: String,
}

/// Bugview API Trait
///
/// This API provides public read-only access to JIRA issues that have been
/// explicitly marked as public through labels.
#[dropshot::api_description]
pub trait BugviewApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    /// Get issue index as JSON
    ///
    /// Returns a paginated list of public issues.
    #[endpoint {
        method = GET,
        path = "/bugview/index.json",
        tags = ["issues"],
    }]
    async fn get_issue_index_json(
        rqctx: RequestContext<Self::Context>,
        query: Query<IssueListQuery>,
    ) -> Result<HttpResponseOk<IssueListResponse>, HttpError>;

    /// Get issue summary as JSON (legacy format)
    ///
    /// Returns issue key, summary, and web URL. This endpoint maintains
    /// backwards compatibility with the original Node.js bugview service.
    #[endpoint {
        method = GET,
        path = "/bugview/json/{key}",
        tags = ["issues"],
    }]
    async fn get_issue_json(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssuePath>,
    ) -> Result<HttpResponseOk<IssueSummary>, HttpError>;

    /// Get full issue details as JSON
    ///
    /// Returns complete issue information including all fields.
    #[endpoint {
        method = GET,
        path = "/bugview/fulljson/{key}",
        tags = ["issues"],
    }]
    async fn get_issue_full_json(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssuePath>,
    ) -> Result<HttpResponseOk<IssueDetails>, HttpError>;

    // ========================================================================
    // HTML Endpoints
    // ========================================================================

    /// Get issue index as HTML
    ///
    /// Returns a paginated HTML view of public issues.
    #[endpoint {
        method = GET,
        path = "/bugview/index.html",
        tags = ["html"],
    }]
    async fn get_issue_index_html(
        rqctx: RequestContext<Self::Context>,
        query: Query<IssueListQuery>,
    ) -> Result<Response<Body>, HttpError>;

    /// Get issues for a specific label as HTML
    ///
    /// Returns a paginated HTML view of issues with the specified label.
    #[endpoint {
        method = GET,
        path = "/bugview/label/{key}",
        tags = ["html"],
    }]
    async fn get_label_index_html(
        rqctx: RequestContext<Self::Context>,
        path: Path<LabelPath>,
        query: Query<IssueListQuery>,
    ) -> Result<Response<Body>, HttpError>;

    /// Get issue details as HTML
    ///
    /// Returns an HTML view of a single issue with full details.
    #[endpoint {
        method = GET,
        path = "/bugview/issue/{key}",
        tags = ["html"],
    }]
    async fn get_issue_html(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssuePath>,
    ) -> Result<Response<Body>, HttpError>;

    // ========================================================================
    // Redirects
    // ========================================================================

    /// Redirect /bugview to /bugview/index.html
    #[endpoint {
        method = GET,
        path = "/bugview",
        tags = ["redirects"],
    }]
    async fn redirect_bugview_root(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError>;
}
