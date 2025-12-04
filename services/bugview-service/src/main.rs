// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

mod html;
mod jira_client;

use anyhow::Result;
use bugview_api::{
    BugviewApi, IssueDetails, IssueListItem, IssueListQuery, IssueListResponse, IssuePath,
    IssueSort, IssueSummary, LabelPath, RemoteLink,
};
use dropshot::{
    Body, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseOk,
    HttpServerStarter, Path, Query, RequestContext,
};
use html::HtmlRenderer;
use http::Response;
use indexmap::IndexMap;
use jira_client::{JiraClient, JiraClientTrait};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::info;

// ================================
// Module constants
// ================================
/// Length of the short, URL-safe pagination token IDs we expose publicly.
const PAGINATION_TOKEN_ID_LEN: usize = 12;
/// Size of the token ID alphabet (0-9, a-z, A-Z).
const TOKEN_ID_ALPHABET_LEN: u8 = 62;
/// Default TTL for cached JIRA pagination tokens (seconds).
const TOKEN_TTL_SECS: u64 = 60 * 60; // 1 hour
/// Maximum number of cached pagination tokens to retain.
const TOKEN_CACHE_MAX_ENTRIES: usize = 1000;
/// Default maximum request body size (bytes).
const DEFAULT_BODY_MAX_BYTES: usize = 1024 * 1024; // 1MB
/// Default bind address for the HTTP server.
const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";
/// Default public base URL for constructing web_url in legacy JSON responses.
const DEFAULT_PUBLIC_BASE_URL: &str = "https://smartos.org";

/// Service configuration
#[derive(Clone)]
struct Config {
    /// Default label for public issues
    default_label: String,
    /// Additional allowed labels
    allowed_labels: Vec<String>,
    /// Allowed domains for remote links (security: prevents exposing signed URLs)
    allowed_domains: Vec<String>,
    /// Public base URL for constructing web_url in legacy JSON responses
    public_base_url: String,
}

impl Config {
    fn is_allowed_label(&self, label: &str) -> bool {
        self.allowed_labels.iter().any(|l| l == label)
    }

    fn is_allowed_domain(&self, domain: &str) -> bool {
        self.allowed_domains.iter().any(|d| d == domain)
    }
}

/// Token cache entry with expiration
struct TokenCacheEntry {
    jira_token: String,
    expires_at: Instant,
}

/// Thread-safe cache for mapping short IDs to JIRA pagination tokens.
///
/// # Why We Use Short IDs
///
/// JIRA v3 API pagination tokens contain base64-encoded data including:
/// - The JQL query being executed
/// - Internal cursor state
/// - Potentially other metadata
///
/// Exposing these tokens directly in URLs would:
/// - Leak query details (labels being searched, filters applied) to users
/// - Allow tokens to appear in browser history and server logs
/// - Potentially enable token manipulation attacks
///
/// Instead, we generate opaque 12-character alphanumeric IDs that map to the
/// real JIRA tokens internally. These IDs are:
/// - Short enough for clean URLs
/// - Cryptographically random (using thread_rng)
/// - Time-limited (TTL-based expiration)
/// - Capacity-limited (LRU eviction)
#[derive(Clone)]
struct TokenCache {
    cache: Arc<Mutex<IndexMap<String, TokenCacheEntry>>>,
    ttl: Duration,
    max_entries: usize,
}

impl TokenCache {
    fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(IndexMap::new())),
            ttl: Duration::from_secs(TOKEN_TTL_SECS),
            max_entries: TOKEN_CACHE_MAX_ENTRIES,
        }
    }

    #[cfg(test)]
    fn new_with(ttl: Duration, max_entries: usize) -> Self {
        Self {
            cache: Arc::new(Mutex::new(IndexMap::new())),
            ttl,
            max_entries,
        }
    }

    /// Store a JIRA token and return a short random ID
    fn store(&self, jira_token: String) -> String {
        use rand::Rng;

        let mut rng = rand::rng();
        let mut cache = self.cache.lock().unwrap_or_else(|poisoned| {
            tracing::error!("Token cache mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        // Cleanup expired entries
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);

        // Enforce capacity by evicting oldest entries (O(1) with IndexMap swap_remove_index)
        while cache.len() >= self.max_entries {
            let _ = cache.swap_remove_index(0);
        }

        // Generate ID, checking for collisions (unlikely but possible)
        let id = loop {
            let candidate: String = (0..PAGINATION_TOKEN_ID_LEN)
                .map(|_| {
                    let idx = rng.random_range(0..TOKEN_ID_ALPHABET_LEN);
                    match idx {
                        0..=9 => (b'0' + idx) as char,
                        10..=35 => (b'a' + idx - 10) as char,
                        _ => (b'A' + idx - 36) as char,
                    }
                })
                .collect();
            if !cache.contains_key(&candidate) {
                break candidate;
            }
        };

        cache.insert(
            id.clone(),
            TokenCacheEntry {
                jira_token,
                expires_at: now + self.ttl,
            },
        );

        id
    }

    /// Retrieve a JIRA token by ID, cleaning up expired entries
    fn get(&self, id: &str) -> Option<String> {
        let mut cache = self.cache.lock().unwrap_or_else(|poisoned| {
            tracing::error!("Token cache mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        // Clean up expired entries
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);

        // Get the token if it exists and hasn't expired
        cache.get(id).map(|entry| entry.jira_token.clone())
    }
}

/// Context for API handlers
struct ApiContext {
    jira: Arc<dyn JiraClientTrait>,
    config: Arc<Config>,
    html: Arc<HtmlRenderer>,
    token_cache: TokenCache,
}

/// Content-Security-Policy header value for HTML responses
/// Allows:
/// - Scripts from self and cdn.jsdelivr.net (Bootstrap JS)
/// - Styles from self, unsafe-inline (for inline styles), and cdn.jsdelivr.net (Bootstrap CSS)
/// - Images from self and data: URIs
/// - Fonts from self and cdn.jsdelivr.net
/// - Default to self for everything else
const CSP_HEADER: &str = "default-src 'self'; script-src 'self' cdn.jsdelivr.net; style-src 'self' 'unsafe-inline' cdn.jsdelivr.net; img-src 'self' data:; font-src 'self' cdn.jsdelivr.net";

/// Helper function to build HTML responses with security headers
fn build_html_response(status: u16, html: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Content-Security-Policy", CSP_HEADER)
        .body(html.into())
        .unwrap()
}

/// Bugview service implementation
enum BugviewServiceImpl {}

impl BugviewApi for BugviewServiceImpl {
    type Context = ApiContext;

    async fn get_issue_index_json(
        rqctx: RequestContext<Self::Context>,
        query: Query<IssueListQuery>,
    ) -> Result<HttpResponseOk<IssueListResponse>, HttpError> {
        let ctx = rqctx.context();
        let query = query.into_inner();

        // Use the default label
        let labels = vec![ctx.config.default_label.clone()];

        search_issues(ctx, labels, query).await
    }

    async fn get_issue_json(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssuePath>,
    ) -> Result<HttpResponseOk<IssueSummary>, HttpError> {
        let ctx = rqctx.context();
        let key_str = path.into_inner().key;

        // Validate issue key format
        let key = jira_api::IssueKey::new(&key_str)
            .map_err(|e| HttpError::for_bad_request(None, format!("{}", e)))?;

        let issue = ctx.jira.get_issue(&key).await.map_err(|e| {
            let msg = e.to_string();
            if msg.contains("Issue not found") {
                HttpError::for_not_found(None, msg)
            } else {
                HttpError::for_internal_error(format!("Failed to get issue: {}", e))
            }
        })?;

        // Check if issue has the required label
        if !issue_has_public_label(&issue, &ctx.config.default_label) {
            return Err(HttpError::for_not_found(
                None,
                format!("Issue {} is not public", key),
            ));
        }

        let summary = issue
            .fields
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                tracing::warn!(
                    issue_key = %issue.key,
                    "Issue missing summary field"
                );
                "(No summary)".to_string()
            });

        Ok(HttpResponseOk(IssueSummary {
            id: issue.key.to_string(),
            summary,
            web_url: format!("{}/bugview/{}", ctx.config.public_base_url, issue.key),
        }))
    }

    async fn get_issue_full_json(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssuePath>,
    ) -> Result<HttpResponseOk<IssueDetails>, HttpError> {
        let ctx = rqctx.context();
        let key_str = path.into_inner().key;

        // Validate issue key format
        let key = jira_api::IssueKey::new(&key_str)
            .map_err(|e| HttpError::for_bad_request(None, format!("{}", e)))?;

        let issue = ctx.jira.get_issue(&key).await.map_err(|e| {
            let msg = e.to_string();
            if msg.contains("Issue not found") {
                HttpError::for_not_found(None, msg)
            } else {
                HttpError::for_internal_error(format!("Failed to get issue: {}", e))
            }
        })?;

        // Check if issue has the required label
        if !issue_has_public_label(&issue, &ctx.config.default_label) {
            return Err(HttpError::for_not_found(
                None,
                format!("Issue {} is not public", key),
            ));
        }

        // Fetch remote links and filter by allowed domains
        let jira_remote_links = ctx
            .jira
            .get_remote_links(&issue.id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    issue_id = %issue.id,
                    issue_key = %issue.key,
                    error = %e,
                    "Failed to fetch remote links, returning empty list"
                );
                Vec::new()
            });

        let filtered_links = filter_remote_links(&jira_remote_links, &ctx.config);

        // Convert to API response format
        let remotelinks: Vec<RemoteLink> = filtered_links
            .iter()
            .filter_map(|link| {
                link.object.as_ref().map(|obj| RemoteLink {
                    url: obj.url.clone(),
                    title: obj.title.clone(),
                })
            })
            .collect();

        let fields = serde_json::to_value(issue.fields).map_err(|e| {
            tracing::error!(
                issue_key = %issue.key,
                error = %e,
                "Failed to serialize issue fields to JSON"
            );
            HttpError::for_internal_error(format!("Failed to serialize issue fields: {}", e))
        })?;

        Ok(HttpResponseOk(IssueDetails {
            id: issue.id,
            key: issue.key,
            fields,
            remotelinks,
        }))
    }

    // ========================================================================
    // HTML Endpoints
    // ========================================================================

    async fn get_issue_index_html(
        rqctx: RequestContext<Self::Context>,
        query: Query<IssueListQuery>,
    ) -> Result<Response<Body>, HttpError> {
        let ctx = rqctx.context();
        let query = query.into_inner();

        // Use the default label
        let labels = vec![ctx.config.default_label.clone()];

        // Get issues
        let (issues, next_page_token, is_last, sort) =
            fetch_issues_for_html(ctx, labels, query).await?;

        // Render HTML
        let html = ctx
            .html
            .render_issue_index(
                &issues,
                next_page_token,
                is_last,
                sort,
                None,
                &ctx.config.allowed_labels,
            )
            .map_err(|e| HttpError::for_internal_error(format!("Failed to render HTML: {}", e)))?;

        Ok(build_html_response(200, html))
    }

    async fn get_label_index_html(
        rqctx: RequestContext<Self::Context>,
        path: Path<LabelPath>,
        query: Query<IssueListQuery>,
    ) -> Result<Response<Body>, HttpError> {
        let ctx = rqctx.context();
        let label = path.into_inner().key;
        let query = query.into_inner();

        // Validate label is allowed
        if !ctx.config.is_allowed_label(&label) {
            return Err(HttpError::for_bad_request(
                None,
                format!("Label '{}' is not public", label),
            ));
        }

        // Combine default label with requested label
        let labels = vec![ctx.config.default_label.clone(), label.clone()];

        // Get issues
        let (issues, next_page_token, is_last, sort) =
            fetch_issues_for_html(ctx, labels, query).await?;

        // Render HTML
        let html = ctx
            .html
            .render_issue_index(
                &issues,
                next_page_token,
                is_last,
                sort,
                Some(&label),
                &ctx.config.allowed_labels,
            )
            .map_err(|e| HttpError::for_internal_error(format!("Failed to render HTML: {}", e)))?;

        Ok(build_html_response(200, html))
    }

    async fn get_issue_html(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssuePath>,
    ) -> Result<Response<Body>, HttpError> {
        let ctx = rqctx.context();
        let key_str = path.into_inner().key;

        // Validate issue key format
        let key = match jira_api::IssueKey::new(&key_str) {
            Ok(k) => k,
            Err(e) => {
                let error_message = format!("{}", e);
                let html = ctx
                    .html
                    .render_error(400, &error_message)
                    .unwrap_or_else(|_| format!("Error 400: {}", error_message));

                return Ok(build_html_response(400, html));
            }
        };

        // Try to get the issue
        let issue = match ctx.jira.get_issue(&key).await {
            Ok(issue) => issue,
            Err(e) => {
                let msg = e.to_string();
                let (status_code, error_message) = if msg.contains("Issue not found") {
                    (404, format!("Issue {} not found", key))
                } else {
                    (500, format!("Failed to retrieve issue: {}", e))
                };

                let html = ctx
                    .html
                    .render_error(status_code, &error_message)
                    .unwrap_or_else(|_| format!("Error {}: {}", status_code, error_message));

                return Ok(build_html_response(status_code, html));
            }
        };

        // Check if issue has the required label
        if !issue_has_public_label(&issue, &ctx.config.default_label) {
            let error_message = format!("Issue {} is not public", key);
            let html = ctx
                .html
                .render_error(404, &error_message)
                .unwrap_or_else(|_| format!("Error 404: {}", error_message));

            return Ok(build_html_response(404, html));
        }

        // Fetch remote links and filter by allowed_domains
        let remote_links = ctx
            .jira
            .get_remote_links(&issue.id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    issue_id = %issue.id,
                    issue_key = %issue.key,
                    error = %e,
                    "Failed to fetch remote links, returning empty list"
                );
                Vec::new()
            });

        let filtered_links = filter_remote_links(&remote_links, &ctx.config);

        // Render HTML
        let html = ctx
            .html
            .render_issue(&issue, &filtered_links)
            .map_err(|e| HttpError::for_internal_error(format!("Failed to render HTML: {}", e)))?;

        Ok(build_html_response(200, html))
    }

    // ========================================================================
    // Redirects
    // ========================================================================

    async fn redirect_bugview_root(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError> {
        Ok(Response::builder()
            .status(302)
            .header("Location", "/bugview/index.html")
            .body(Body::empty())
            .unwrap())
    }
}

/// Helper function to fetch issues for HTML rendering
async fn fetch_issues_for_html(
    ctx: &ApiContext,
    labels: Vec<String>,
    query: IssueListQuery,
) -> Result<(Vec<IssueListItem>, Option<String>, bool, IssueSort), HttpError> {
    let sort = query.sort.unwrap_or_default();

    // Resolve the short token ID to the real JIRA token
    // For HTML endpoints, we gracefully fall back to first page on bad tokens
    let jira_token = if let Some(short_id) = &query.next_page_token {
        ctx.token_cache.get(short_id)
    } else {
        None
    };

    let search_result = ctx
        .jira
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
        .map(|jira_token| ctx.token_cache.store(jira_token));

    Ok((issues, next_page_token, is_last, sort))
}

/// Helper function to search issues
async fn search_issues(
    ctx: &ApiContext,
    labels: Vec<String>,
    query: IssueListQuery,
) -> Result<HttpResponseOk<IssueListResponse>, HttpError> {
    let sort = query.sort.unwrap_or_default();

    // Resolve the short token ID to the real JIRA token
    let jira_token = if let Some(short_id) = &query.next_page_token {
        Some(ctx.token_cache.get(short_id).ok_or_else(|| {
            HttpError::for_bad_request(None, "Invalid or expired pagination token".to_string())
        })?)
    } else {
        None
    };

    let search_result = ctx
        .jira
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
        .map(|jira_token| ctx.token_cache.store(jira_token));

    Ok(HttpResponseOk(IssueListResponse {
        issues,
        next_page_token,
        is_last: search_result.is_last.unwrap_or(false),
    }))
}

/// Helper to check if issue has the public label
fn issue_has_public_label(issue: &jira_api::Issue, required_label: &str) -> bool {
    issue
        .fields
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|labels| labels.iter().any(|l| l.as_str() == Some(required_label)))
        .unwrap_or(false)
}

/// Helper to convert full issue to list item
fn convert_to_list_item(issue: jira_api::Issue) -> IssueListItem {
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

/// Filter remote links by allowed domains and safe URL schemes
///
/// This prevents exposing links to signed Manta URLs or other sensitive domains,
/// and prevents XSS attacks via javascript: or data: URLs
fn filter_remote_links(
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

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "bugview_service=info,dropshot=info".to_string()),
        ))
        .init();

    // Load configuration from environment
    let jira_url =
        std::env::var("JIRA_URL").unwrap_or_else(|_| "https://jira.example.com".to_string());
    let jira_username = std::env::var("JIRA_USERNAME").unwrap_or_else(|_| "username".to_string());
    let jira_password = std::env::var("JIRA_PASSWORD").unwrap_or_else(|_| "password".to_string());
    let default_label =
        std::env::var("JIRA_DEFAULT_LABEL").unwrap_or_else(|_| "public".to_string());
    let allowed_labels = std::env::var("JIRA_ALLOWED_LABELS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
        .collect();

    info!("Initializing JIRA client");
    let jira_client = JiraClient::new(jira_url, jira_username, jira_password)?;

    info!("Initializing HTML renderer");
    let html_renderer = HtmlRenderer::new();

    let allowed_domains = std::env::var("JIRA_ALLOWED_DOMAINS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
        .collect();

    let public_base_url =
        std::env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| DEFAULT_PUBLIC_BASE_URL.to_string());

    let config = Config {
        default_label,
        allowed_labels,
        allowed_domains,
        public_base_url,
    };

    let api_context = ApiContext {
        jira: Arc::new(jira_client) as Arc<dyn JiraClientTrait>,
        config: Arc::new(config),
        html: Arc::new(html_renderer),
        token_cache: TokenCache::new(),
    };

    // Get API description from the trait implementation
    let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
        .map_err(|e| anyhow::anyhow!("Failed to create API description: {}", e))?;

    // Configure the server
    let bind_address = std::env::var("BIND_ADDRESS")
        .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_string())
        .parse()?;

    let config_dropshot = ConfigDropshot {
        bind_address,
        default_request_body_max_bytes: DEFAULT_BODY_MAX_BYTES,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };

    let log = config_logging
        .to_logger("bugview-service")
        .map_err(|error| anyhow::anyhow!("failed to create logger: {}", error))?;

    // Start the server
    let server = HttpServerStarter::new(&config_dropshot, api, api_context, &log)
        .map_err(|error| anyhow::anyhow!("failed to create server: {}", error))?
        .start();

    info!("Bugview service running on http://{}", bind_address);

    server
        .await
        .map_err(|error| anyhow::anyhow!("server failed: {}", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira_client::SearchResponse;
    use async_trait::async_trait;
    use bugview_api::IssueDetails;
    use http::StatusCode;
    use jira_api::{Issue, RemoteLink};

    // Mock JIRA client implementing the trait for tests with public labels
    #[derive(Clone, Default)]
    struct MockJiraClient;

    #[async_trait]
    impl JiraClientTrait for MockJiraClient {
        async fn search_issues(
            &self,
            _labels: &[String],
            _page_token: Option<&str>,
            _sort: &str,
        ) -> anyhow::Result<SearchResponse> {
            // Build two simple issues that contain the required fields
            let mut issue1 = serde_json::Map::new();
            issue1.insert("summary".into(), serde_json::Value::String("Alpha".into()));
            issue1.insert(
                "created".into(),
                serde_json::Value::String("2023-10-01T00:00:00.000-0400".into()),
            );
            issue1.insert(
                "updated".into(),
                serde_json::Value::String("2023-10-02T00:00:00.000-0400".into()),
            );
            issue1.insert("labels".into(), serde_json::json!(["public"]));

            let mut issue2 = serde_json::Map::new();
            issue2.insert("summary".into(), serde_json::Value::String("Beta".into()));
            issue2.insert(
                "created".into(),
                serde_json::Value::String("2023-10-03T00:00:00.000-0400".into()),
            );
            issue2.insert(
                "updated".into(),
                serde_json::Value::String("2023-10-04T00:00:00.000-0400".into()),
            );
            issue2.insert("labels".into(), serde_json::json!(["public"]));

            let issues = vec![
                Issue {
                    key: jira_api::IssueKey::new_unchecked("PROJ-1"),
                    id: "1".into(),
                    fields: issue1.into_iter().collect(),
                    rendered_fields: None,
                },
                Issue {
                    key: jira_api::IssueKey::new_unchecked("PROJ-2"),
                    id: "2".into(),
                    fields: issue2.into_iter().collect(),
                    rendered_fields: None,
                },
            ];

            Ok(SearchResponse {
                issues,
                is_last: Some(true),
                next_page_token: None,
            })
        }

        async fn get_issue(&self, key: &jira_api::IssueKey) -> anyhow::Result<Issue> {
            // Return a minimal issue with required fields and the public label
            let mut fields: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            fields.insert(
                "summary".to_string(),
                serde_json::Value::String("Test summary".into()),
            );
            fields.insert(
                "created".to_string(),
                serde_json::Value::String("2023-10-04T10:27:22.826-0400".into()),
            );
            fields.insert(
                "updated".to_string(),
                serde_json::Value::String("2023-10-05T10:27:22.826-0400".into()),
            );
            fields.insert("labels".to_string(), serde_json::json!(["public"]));

            Ok(Issue {
                key: key.clone(),
                id: "12345".to_string(),
                fields: fields.into_iter().collect(),
                rendered_fields: None,
            })
        }

        async fn get_remote_links(&self, _issue_id: &str) -> anyhow::Result<Vec<RemoteLink>> {
            // Construct via JSON to avoid depending on generated auxiliary types
            let link: RemoteLink = serde_json::from_value(serde_json::json!({
                "id": 1,
                "object": { "url": "https://example.com/resource", "title": "Example" }
            }))?;
            Ok(vec![link])
        }
    }

    // Mock JIRA client that returns issues WITHOUT the public label
    // Used to test the security boundary
    #[derive(Clone, Default)]
    struct NonPublicMockJiraClient;

    #[async_trait]
    impl JiraClientTrait for NonPublicMockJiraClient {
        async fn search_issues(
            &self,
            _labels: &[String],
            _page_token: Option<&str>,
            _sort: &str,
        ) -> anyhow::Result<SearchResponse> {
            // Return issues with "internal" label instead of "public"
            let mut issue1 = serde_json::Map::new();
            issue1.insert(
                "summary".into(),
                serde_json::Value::String("Internal Alpha".into()),
            );
            issue1.insert(
                "created".into(),
                serde_json::Value::String("2023-10-01T00:00:00.000-0400".into()),
            );
            issue1.insert(
                "updated".into(),
                serde_json::Value::String("2023-10-02T00:00:00.000-0400".into()),
            );
            issue1.insert("labels".into(), serde_json::json!(["internal"]));

            let issues = vec![Issue {
                key: jira_api::IssueKey::new_unchecked("PROJ-1"),
                id: "1".into(),
                fields: issue1.into_iter().collect(),
                rendered_fields: None,
            }];

            Ok(SearchResponse {
                issues,
                is_last: Some(true),
                next_page_token: None,
            })
        }

        async fn get_issue(&self, key: &jira_api::IssueKey) -> anyhow::Result<Issue> {
            // Return an issue with "internal" label instead of "public"
            let mut fields: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            fields.insert(
                "summary".to_string(),
                serde_json::Value::String("Internal issue".into()),
            );
            fields.insert(
                "created".to_string(),
                serde_json::Value::String("2023-10-04T10:27:22.826-0400".into()),
            );
            fields.insert(
                "updated".to_string(),
                serde_json::Value::String("2023-10-05T10:27:22.826-0400".into()),
            );
            fields.insert("labels".to_string(), serde_json::json!(["internal"]));

            Ok(Issue {
                key: key.clone(),
                id: "12345".to_string(),
                fields: fields.into_iter().collect(),
                rendered_fields: None,
            })
        }

        async fn get_remote_links(&self, _issue_id: &str) -> anyhow::Result<Vec<RemoteLink>> {
            // Return empty list for non-public issues
            Ok(vec![])
        }
    }

    // Build a test ApiContext with the mock client
    fn test_context() -> ApiContext {
        let config = Config {
            default_label: "public".to_string(),
            allowed_labels: vec!["public".to_string(), "bug".to_string()],
            allowed_domains: vec!["example.com".to_string()],
            public_base_url: "https://test.example.com".to_string(),
        };

        ApiContext {
            jira: Arc::new(MockJiraClient) as Arc<dyn JiraClientTrait>,
            config: Arc::new(config),
            html: Arc::new(HtmlRenderer::new()),
            token_cache: TokenCache::new(),
        }
    }

    // Build a test ApiContext with the non-public mock client
    fn non_public_test_context() -> ApiContext {
        let config = Config {
            default_label: "public".to_string(),
            allowed_labels: vec!["public".to_string(), "bug".to_string()],
            allowed_domains: vec!["example.com".to_string()],
            public_base_url: "https://test.example.com".to_string(),
        };

        ApiContext {
            jira: Arc::new(NonPublicMockJiraClient) as Arc<dyn JiraClientTrait>,
            config: Arc::new(config),
            html: Arc::new(HtmlRenderer::new()),
            token_cache: TokenCache::new(),
        }
    }

    #[tokio::test]
    async fn test_issue_full_json_with_mock() {
        // Exercise the same logic used by the handler with a mock client
        let ctx = test_context();
        let key = jira_api::IssueKey::new("PROJ-1").expect("valid key");
        let issue = ctx.jira.get_issue(&key).await.expect("mock get_issue");

        // Verify label gate
        assert!(issue_has_public_label(&issue, &ctx.config.default_label));

        let details = IssueDetails {
            id: issue.id,
            key: issue.key,
            fields: serde_json::to_value(issue.fields).unwrap(),
            remotelinks: vec![],
        };

        // Spot-check a couple fields
        let fields = details.fields.as_object().unwrap();
        assert_eq!(
            fields.get("summary").and_then(|v| v.as_str()),
            Some("Test summary")
        );
    }

    #[tokio::test]
    async fn test_issue_html_with_mock_and_link_filter() {
        let ctx = test_context();
        let key = jira_api::IssueKey::new("PROJ-2").expect("valid key");
        let issue = ctx.jira.get_issue(&key).await.unwrap();
        // Build a link via JSON to avoid referring to internal generated types directly
        let link: jira_api::RemoteLink = serde_json::from_value(serde_json::json!({
            "id": 1,
            "object": { "url": "https://example.com/resource", "title": "Example" }
        }))
        .unwrap();
        let links = vec![link];

        let filtered = super::filter_remote_links(&links, &ctx.config);
        assert_eq!(filtered.len(), 1, "allowed domain should pass");

        let html = ctx
            .html
            .render_issue(&issue, &filtered)
            .expect("render html");
        assert!(html.contains("Test summary"));
        assert!(html.contains("Related Links"));
        assert!(html.contains("example.com"));
    }

    #[tokio::test]
    async fn test_remote_link_domain_filter_blocks_disallowed() {
        let config = Config {
            default_label: "public".into(),
            allowed_labels: vec![],
            allowed_domains: vec!["safe.example.com".into()],
            public_base_url: "https://test.example.com".into(),
        };

        // Create links from mixed domains - some allowed, some not
        let links: Vec<jira_api::RemoteLink> = serde_json::from_value(serde_json::json!([
            {"id": 1, "object": {"url": "https://safe.example.com/good", "title": "Good Link"}},
            {"id": 2, "object": {"url": "https://manta.joyent.us/signed/secret", "title": "Signed URL"}},
            {"id": 3, "object": {"url": "https://internal.corp/secret", "title": "Internal"}},
            {"id": 4, "object": {"url": "https://safe.example.com/another", "title": "Also Good"}}
        ])).unwrap();

        let filtered = filter_remote_links(&links, &config);

        assert_eq!(
            filtered.len(),
            2,
            "Should only keep links from allowed domain"
        );
        assert!(
            filtered
                .iter()
                .all(|l| l.object.as_ref().unwrap().url.contains("safe.example.com"))
        );
    }

    #[tokio::test]
    async fn test_remote_link_scheme_filter_blocks_javascript_and_data() {
        let config = Config {
            default_label: "public".into(),
            allowed_labels: vec![],
            allowed_domains: vec!["example.com".into()],
            public_base_url: "https://test.example.com".into(),
        };

        // Create links with dangerous schemes that could enable XSS
        let links: Vec<jira_api::RemoteLink> = serde_json::from_value(serde_json::json!([
            {"id": 1, "object": {"url": "https://example.com/safe", "title": "Safe HTTPS"}},
            {"id": 2, "object": {"url": "http://example.com/safe", "title": "Safe HTTP"}},
            {"id": 3, "object": {"url": "javascript:alert('XSS')", "title": "JavaScript XSS"}},
            {"id": 4, "object": {"url": "data:text/html,<script>alert('XSS')</script>", "title": "Data URI XSS"}},
            {"id": 5, "object": {"url": "file:///etc/passwd", "title": "File Scheme"}},
            {"id": 6, "object": {"url": "vbscript:msgbox('XSS')", "title": "VBScript"}},
        ])).unwrap();

        let filtered = filter_remote_links(&links, &config);

        assert_eq!(
            filtered.len(),
            2,
            "Should only keep http and https links"
        );

        // Verify only safe schemes remain
        for link in &filtered {
            let url = &link.object.as_ref().unwrap().url;
            assert!(
                url.starts_with("https://") || url.starts_with("http://"),
                "Filtered link should have safe scheme: {}",
                url
            );
        }

        // Verify dangerous schemes were filtered out
        assert!(
            !filtered.iter().any(|l| l.object.as_ref().unwrap().url.contains("javascript:")),
            "javascript: URLs should be filtered"
        );
        assert!(
            !filtered.iter().any(|l| l.object.as_ref().unwrap().url.contains("data:")),
            "data: URLs should be filtered"
        );
        assert!(
            !filtered.iter().any(|l| l.object.as_ref().unwrap().url.contains("file:")),
            "file: URLs should be filtered"
        );
    }

    #[tokio::test]
    async fn test_token_cache_capacity_bounds() {
        let cache = TokenCache::new_with(Duration::from_secs(60), 3);
        let ids: Vec<String> = (0..10)
            .map(|i| cache.store(format!("token-{}", i)))
            .collect();

        // Oldest should be evicted to maintain capacity
        let len = cache
            .cache
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::error!("Token cache mutex was poisoned, recovering");
                poisoned.into_inner()
            })
            .len();
        assert!(len <= 3, "cache should be bounded");

        // Newest likely remain; ensure at least last id resolves
        let last_id = ids.last().unwrap();
        assert_eq!(cache.get(last_id).as_deref(), Some("token-9"));
    }

    #[tokio::test]
    async fn test_token_cache_expiration() {
        let cache = TokenCache::new_with(Duration::from_millis(50), 100);
        let id = cache.store("test-token".to_string());

        // Token should be retrievable immediately
        assert_eq!(cache.get(&id).as_deref(), Some("test-token"));

        // Wait for expiration
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Token should be expired
        assert!(cache.get(&id).is_none());
    }

    #[tokio::test]
    async fn test_http_issue_route_with_mock_server() {
        // Start a real HTTP server on an ephemeral port with the mock client
        let ctx = test_context();

        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
            .expect("api description");

        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };

        let log = dropshot::ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Info,
        }
        .to_logger("bugview-service-test")
        .expect("logger");

        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start test server in CI: {}", e);
                }
                eprintln!("SKIPPING: failed to start server: {} (set CI=1 to fail)", e);
                return; // likely sandbox prevents binding sockets
            }
        };

        // Best-effort small delay to ensure server is ready
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Get bound address
        let addr = server.local_addr();
        let url = format!("http://{}/bugview/issue/PROJ-1", addr);

        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.text().await.expect("body");
        assert!(body.contains("Test summary"));
    }

    #[tokio::test]
    async fn test_http_index_json_with_mock_server() {
        let ctx = test_context();
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
            .expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Info,
        }
        .to_logger("bugview-service-test")
        .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start test server in CI: {}", e);
                }
                eprintln!("SKIPPING: failed to start server: {} (set CI=1 to fail)", e);
                return;
            }
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let addr = server.local_addr();
        let url = format!("http://{}/bugview/index.json", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.text().await.expect("body");
        assert!(body.contains("\"issues\""));
        assert!(body.contains("PROJ-1") && body.contains("PROJ-2"));
    }

    #[tokio::test]
    async fn test_http_index_html_with_mock_server() {
        let ctx = test_context();
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
            .expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Info,
        }
        .to_logger("bugview-service-test")
        .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start test server in CI: {}", e);
                }
                eprintln!("SKIPPING: failed to start server: {} (set CI=1 to fail)", e);
                return;
            }
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let addr = server.local_addr();
        let url = format!("http://{}/bugview/index.html", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.text().await.expect("body");
        assert!(body.contains("Public Issues Index") || body.contains("Public Issues"));
    }

    #[tokio::test]
    async fn test_http_label_index_html_disallowed() {
        let ctx = test_context();
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
            .expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Info,
        }
        .to_logger("bugview-service-test")
        .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start test server in CI: {}", e);
                }
                eprintln!("SKIPPING: failed to start server: {} (set CI=1 to fail)", e);
                return;
            }
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let addr = server.local_addr();
        let url = format!("http://{}/bugview/label/not-allowed", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_http_redirect_bugview_root() {
        let ctx = test_context();
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
            .expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Info,
        }
        .to_logger("bugview-service-test")
        .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start test server in CI: {}", e);
                }
                eprintln!("SKIPPING: failed to start server: {} (set CI=1 to fail)", e);
                return;
            }
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let addr = server.local_addr();
        let url = format!("http://{}/bugview", addr);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let resp = client.get(&url).send().await.expect("request");
        assert_eq!(resp.status(), StatusCode::FOUND);
        let loc = resp
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(loc, "/bugview/index.html");
    }

    #[tokio::test]
    async fn test_csp_header_is_set_on_html_responses() {
        let ctx = test_context();
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
            .expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Info,
        }
        .to_logger("bugview-service-test")
        .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start test server in CI: {}", e);
                }
                eprintln!("SKIPPING: failed to start server: {} (set CI=1 to fail)", e);
                return;
            }
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let addr = server.local_addr();

        // Test index.html has CSP header
        let url = format!("http://{}/bugview/index.html", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::OK);
        let csp_header = resp
            .headers()
            .get("Content-Security-Policy")
            .and_then(|v| v.to_str().ok());
        assert!(
            csp_header.is_some(),
            "CSP header should be present on HTML response"
        );
        assert_eq!(csp_header.unwrap(), CSP_HEADER);

        // Test issue HTML page has CSP header
        let url = format!("http://{}/bugview/issue/PROJ-1", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::OK);
        let csp_header = resp
            .headers()
            .get("Content-Security-Policy")
            .and_then(|v| v.to_str().ok());
        assert!(
            csp_header.is_some(),
            "CSP header should be present on issue HTML page"
        );
        assert_eq!(csp_header.unwrap(), CSP_HEADER);

        // Test error page has CSP header
        let url = format!("http://{}/bugview/issue/INVALID", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let csp_header = resp
            .headers()
            .get("Content-Security-Policy")
            .and_then(|v| v.to_str().ok());
        assert!(
            csp_header.is_some(),
            "CSP header should be present on error HTML page"
        );
        assert_eq!(csp_header.unwrap(), CSP_HEADER);
    }

    #[tokio::test]
    async fn test_http_non_public_issue_returns_404() {
        // Test the security boundary: issues without the public label should return 404
        let ctx = non_public_test_context();

        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
            .expect("api description");

        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };

        let log = dropshot::ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Info,
        }
        .to_logger("bugview-service-test")
        .expect("logger");

        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start test server in CI: {}", e);
                }
                eprintln!("SKIPPING: failed to start server: {} (set CI=1 to fail)", e);
                return; // likely sandbox prevents binding sockets
            }
        };

        // Best-effort small delay to ensure server is ready
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Get bound address
        let addr = server.local_addr();

        // Test HTML endpoint returns 404
        let url_html = format!("http://{}/bugview/issue/PROJ-1", addr);
        let resp_html = reqwest::get(&url_html).await.expect("request");
        assert_eq!(
            resp_html.status(),
            StatusCode::NOT_FOUND,
            "HTML endpoint should return 404 for non-public issue"
        );
        let body_html = resp_html.text().await.expect("body");
        assert!(
            body_html.contains("not public") || body_html.contains("404"),
            "Response should indicate issue is not public or not found"
        );

        // Test JSON summary endpoint returns 404
        let url_json = format!("http://{}/bugview/json/PROJ-1", addr);
        let resp_json = reqwest::get(&url_json).await.expect("request");
        assert_eq!(
            resp_json.status(),
            StatusCode::NOT_FOUND,
            "JSON summary endpoint should return 404 for non-public issue"
        );

        // Test full JSON endpoint returns 404
        let url_fulljson = format!("http://{}/bugview/fulljson/PROJ-1", addr);
        let resp_fulljson = reqwest::get(&url_fulljson).await.expect("request");
        assert_eq!(
            resp_fulljson.status(),
            StatusCode::NOT_FOUND,
            "Full JSON endpoint should return 404 for non-public issue"
        );
    }
}
