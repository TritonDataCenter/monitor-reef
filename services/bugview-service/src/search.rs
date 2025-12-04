// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Issue search and conversion helpers.
//!
//! This module contains functions for searching JIRA issues, converting
//! between JIRA and bugview API types, and filtering remote links.

use bugview_api::{IssueListItem, IssueListQuery, IssueListResponse, IssueSort};
use dropshot::{HttpError, HttpResponseOk};

use crate::Config;
use crate::jira_client::JiraClientTrait;
use crate::token_cache::TokenCache;

/// Helper function to fetch issues for HTML rendering.
///
/// This variant gracefully falls back to the first page on invalid/expired tokens,
/// which provides a better UX for HTML pages where users might have stale bookmarks.
pub async fn fetch_issues_for_html(
    jira: &dyn JiraClientTrait,
    token_cache: &TokenCache,
    labels: Vec<String>,
    query: IssueListQuery,
) -> Result<(Vec<IssueListItem>, Option<String>, bool, IssueSort), HttpError> {
    let sort = query.sort.unwrap_or_default();

    // Resolve the short token ID to the real JIRA token
    // For HTML endpoints, we gracefully fall back to first page on bad tokens
    let jira_token = if let Some(short_id) = &query.next_page_token {
        token_cache.get(short_id)
    } else {
        None
    };

    let search_result = jira
        .search_issues(&labels, jira_token.as_deref(), sort.as_str())
        .await
        .map_err(|e| HttpError::for_internal_error(format!("Failed to search issues: {}", e)))?;

    let issues: Vec<IssueListItem> = search_result
        .issues
        .into_iter()
        .map(convert_to_list_item)
        .collect();

    // Extract pagination info from JIRA response
    let is_last = search_result.is_last.unwrap_or(false);

    // Store JIRA's token in cache and return short ID instead
    let next_page_token = search_result
        .next_page_token
        .map(|jira_token| token_cache.store(jira_token));

    Ok((issues, next_page_token, is_last, sort))
}

/// Helper function to search issues for JSON API responses.
///
/// This variant returns an error for invalid/expired tokens, which is the
/// correct behavior for programmatic API clients that should handle errors.
pub async fn search_issues(
    jira: &dyn JiraClientTrait,
    token_cache: &TokenCache,
    labels: Vec<String>,
    query: IssueListQuery,
) -> Result<HttpResponseOk<IssueListResponse>, HttpError> {
    let sort = query.sort.unwrap_or_default();

    // Resolve the short token ID to the real JIRA token
    let jira_token = if let Some(short_id) = &query.next_page_token {
        Some(token_cache.get(short_id).ok_or_else(|| {
            HttpError::for_bad_request(None, "Invalid or expired pagination token".to_string())
        })?)
    } else {
        None
    };

    let search_result = jira
        .search_issues(&labels, jira_token.as_deref(), sort.as_str())
        .await
        .map_err(|e| HttpError::for_internal_error(format!("Failed to search issues: {}", e)))?;

    let issues: Vec<IssueListItem> = search_result
        .issues
        .into_iter()
        .map(convert_to_list_item)
        .collect();

    // Store JIRA's token in cache and return short ID instead
    let next_page_token = search_result
        .next_page_token
        .map(|jira_token| token_cache.store(jira_token));

    // Use constructor to ensure is_last and next_page_token are consistent
    Ok(HttpResponseOk(IssueListResponse::new(
        issues,
        next_page_token,
    )))
}

/// Check if an issue has the required public label.
pub fn issue_has_public_label(issue: &jira_api::Issue, required_label: &str) -> bool {
    issue
        .fields
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|labels| labels.iter().any(|l| l.as_str() == Some(required_label)))
        .unwrap_or(false)
}

/// Convert a full JIRA issue to a list item for the index.
pub fn convert_to_list_item(issue: jira_api::Issue) -> IssueListItem {
    let summary = issue
        .fields
        .get("summary")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            tracing::warn!(
                issue_key = %issue.key,
                "Issue missing summary field in list item"
            );
            "(No summary)".to_string()
        });

    let status = issue
        .fields
        .get("status")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            tracing::warn!(
                issue_key = %issue.key,
                "Issue missing status field"
            );
            "Unknown".to_string()
        });

    let resolution = issue
        .fields
        .get("resolution")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let updated = issue
        .fields
        .get("updated")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let created = issue
        .fields
        .get("created")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    IssueListItem {
        key: issue.key,
        summary,
        status,
        resolution,
        updated,
        created,
    }
}

/// Filter remote links by allowed domains and safe URL schemes.
///
/// # Security
///
/// This function implements an **allowlist-based** security model:
///
/// - **URL Schemes**: Only `http://` and `https://` URLs are allowed.
///   All other schemes (javascript:, data:, file:, vbscript:, etc.) are blocked
///   to prevent XSS attacks.
///
/// - **Domains**: Only domains explicitly listed in configuration are allowed.
///   This prevents exposing links to signed Manta URLs or other sensitive internal domains.
///
/// Links that fail either check are silently filtered out.
pub fn filter_remote_links(
    links: &[jira_api::RemoteLink],
    config: &Config,
) -> Vec<jira_api::RemoteLink> {
    links
        .iter()
        .filter(|link| {
            if let Some(obj) = &link.object
                && let Ok(url) = url::Url::parse(&obj.url)
                && let Some(domain) = url.host_str()
            {
                // Only allow http and https schemes to prevent XSS
                let scheme = url.scheme();
                if scheme != "http" && scheme != "https" {
                    return false;
                }

                // Check domain is allowed
                return config.is_allowed_domain(domain);
            }
            false
        })
        .cloned()
        .collect()
}
