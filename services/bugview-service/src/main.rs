// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

mod html;
mod jira_client;
mod search;
mod token_cache;

use anyhow::Result;
use bugview_api::{
    BugviewApi, IssueDetails, IssueListQuery, IssueListResponse, IssuePath, IssueSummary,
    LabelPath, RemoteLink,
};
use dropshot::{
    Body, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseOk,
    HttpServerStarter, Path, Query, RequestContext,
};
use html::HtmlRenderer;
use http::Response;
use jira_client::{JiraClient, JiraClientTrait};
use search::{fetch_issues_for_html, filter_remote_links, issue_has_public_label, search_issues};
use std::sync::Arc;
use token_cache::TokenCache;
use tracing::info;

// ================================
// Module constants
// ================================
/// Default maximum request body size (bytes).
const DEFAULT_BODY_MAX_BYTES: usize = 1024 * 1024; // 1MB
/// Default bind address for the HTTP server.
const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";
/// Default public base URL for constructing web_url in legacy JSON responses.
const DEFAULT_PUBLIC_BASE_URL: &str = "https://smartos.org";

/// Service configuration
#[derive(Clone)]
pub(crate) struct Config {
    /// Default label for public issues
    pub(crate) default_label: String,
    /// Additional allowed labels
    pub(crate) allowed_labels: Vec<String>,
    /// Allowed domains for remote links (security: prevents exposing signed URLs)
    allowed_domains: Vec<String>,
    /// Public base URL for constructing web_url in legacy JSON responses
    pub(crate) public_base_url: String,
}

impl Config {
    pub(crate) fn is_allowed_label(&self, label: &str) -> bool {
        self.allowed_labels.iter().any(|l| l == label)
    }

    pub(crate) fn is_allowed_domain(&self, domain: &str) -> bool {
        self.allowed_domains.iter().any(|d| d == domain)
    }
}

/// Context for API handlers
struct ApiContext {
    jira: Arc<dyn JiraClientTrait>,
    config: Config,
    html: HtmlRenderer,
    token_cache: TokenCache,
}

/// Content-Security-Policy header value for HTML responses
/// Allows:
/// - Scripts from self, unsafe-inline (for theme detection), and cdn.jsdelivr.net (Bootstrap JS)
/// - Styles from self, unsafe-inline (for inline styles), and cdn.jsdelivr.net (Bootstrap CSS)
/// - Images from self and data: URIs
/// - Fonts from self and cdn.jsdelivr.net
/// - Default to self for everything else
const CSP_HEADER: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' cdn.jsdelivr.net; style-src 'self' 'unsafe-inline' cdn.jsdelivr.net; img-src 'self' data:; font-src 'self' cdn.jsdelivr.net";

/// Helper function to build HTML responses with security headers
fn build_html_response(status: u16, html: String) -> Result<Response<Body>, HttpError> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Content-Security-Policy", CSP_HEADER)
        .body(html.into())
        .map_err(|e| HttpError::for_internal_error(format!("Failed to build response: {}", e)))
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

        search_issues(ctx.jira.as_ref(), &ctx.token_cache, labels, query).await
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
            if msg.contains("Issue not found") || msg.contains("404") {
                // Safe to expose - user is asking for an issue that doesn't exist
                HttpError::for_not_found(None, format!("Issue {} not found", key))
            } else {
                // Log full error but return generic message to avoid exposing internals
                tracing::error!(issue_key = %key, error = %e, "Failed to get issue from JIRA");
                HttpError::for_internal_error(
                    "Failed to retrieve issue. Please try again later.".to_string(),
                )
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
            if msg.contains("Issue not found") || msg.contains("404") {
                // Safe to expose - user is asking for an issue that doesn't exist
                HttpError::for_not_found(None, format!("Issue {} not found", key))
            } else {
                // Log full error but return generic message to avoid exposing internals
                tracing::error!(issue_key = %key, error = %e, "Failed to get issue from JIRA");
                HttpError::for_internal_error(
                    "Failed to retrieve issue. Please try again later.".to_string(),
                )
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
                let err_str = e.to_string();
                // Escalate to error level for auth failures and rate limiting
                // as these indicate operational issues that need attention
                if err_str.contains("401")
                    || err_str.contains("403")
                    || err_str.to_lowercase().contains("unauthorized")
                    || err_str.to_lowercase().contains("forbidden")
                {
                    tracing::error!(
                        issue_id = %issue.id,
                        issue_key = %issue.key,
                        error = %e,
                        "Authentication/authorization failure fetching remote links - check JIRA credentials"
                    );
                } else if err_str.contains("429")
                    || err_str.to_lowercase().contains("too many requests")
                {
                    tracing::error!(
                        issue_id = %issue.id,
                        issue_key = %issue.key,
                        error = %e,
                        "Rate limited by JIRA when fetching remote links"
                    );
                } else {
                    tracing::warn!(
                        issue_id = %issue.id,
                        issue_key = %issue.key,
                        error = %e,
                        "Failed to fetch remote links, returning empty list"
                    );
                }
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
            fetch_issues_for_html(ctx.jira.as_ref(), &ctx.token_cache, labels, query).await?;

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

        build_html_response(200, html)
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
            fetch_issues_for_html(ctx.jira.as_ref(), &ctx.token_cache, labels, query).await?;

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

        build_html_response(200, html)
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
                let html =
                    ctx.html
                        .render_error(400, &error_message)
                        .unwrap_or_else(|template_err| {
                            tracing::error!(
                                error = %template_err,
                                "Failed to render error page template"
                            );
                            format!("Error 400: {}", error_message)
                        });

                return build_html_response(400, html);
            }
        };

        // Try to get the issue
        let issue = match ctx.jira.get_issue(&key).await {
            Ok(issue) => issue,
            Err(e) => {
                let msg = e.to_string();
                let (status_code, error_message) = if msg.contains("Issue not found")
                    || msg.contains("404")
                {
                    // Safe to expose - user is asking for an issue that doesn't exist
                    (404, format!("Issue {} not found", key))
                } else {
                    // Log full error but return generic message to avoid exposing internals
                    tracing::error!(issue_key = %key, error = %e, "Failed to get issue from JIRA");
                    (
                        500,
                        "Failed to retrieve issue. Please try again later.".to_string(),
                    )
                };

                let html = ctx
                    .html
                    .render_error(status_code, &error_message)
                    .unwrap_or_else(|template_err| {
                        tracing::error!(
                            error = %template_err,
                            status_code = status_code,
                            "Failed to render error page template"
                        );
                        format!("Error {}: {}", status_code, error_message)
                    });

                return build_html_response(status_code, html);
            }
        };

        // Check if issue has the required label
        if !issue_has_public_label(&issue, &ctx.config.default_label) {
            let error_message = format!("Issue {} is not public", key);
            let html = ctx
                .html
                .render_error(404, &error_message)
                .unwrap_or_else(|template_err| {
                    tracing::error!(
                        error = %template_err,
                        "Failed to render error page template"
                    );
                    format!("Error 404: {}", error_message)
                });

            return build_html_response(404, html);
        }

        // Fetch remote links and filter by allowed_domains
        // Track whether fetch failed so we can show a warning to users
        let (remote_links, remote_links_error) = match ctx.jira.get_remote_links(&issue.id).await {
            Ok(links) => (links, false),
            Err(e) => {
                let err_str = e.to_string();
                // Escalate to error level for auth failures and rate limiting
                // as these indicate operational issues that need attention
                if err_str.contains("401")
                    || err_str.contains("403")
                    || err_str.to_lowercase().contains("unauthorized")
                    || err_str.to_lowercase().contains("forbidden")
                {
                    tracing::error!(
                        issue_id = %issue.id,
                        issue_key = %issue.key,
                        error = %e,
                        "Authentication/authorization failure fetching remote links - check JIRA credentials"
                    );
                } else if err_str.contains("429")
                    || err_str.to_lowercase().contains("too many requests")
                {
                    tracing::error!(
                        issue_id = %issue.id,
                        issue_key = %issue.key,
                        error = %e,
                        "Rate limited by JIRA when fetching remote links"
                    );
                } else {
                    tracing::warn!(
                        issue_id = %issue.id,
                        issue_key = %issue.key,
                        error = %e,
                        "Failed to fetch remote links, will show warning to user"
                    );
                }
                (Vec::new(), true)
            }
        };

        let filtered_links = filter_remote_links(&remote_links, &ctx.config);

        // Render HTML (pass error flag to show warning if links couldn't be loaded)
        let html = ctx
            .html
            .render_issue(&issue, &filtered_links, remote_links_error)
            .map_err(|e| HttpError::for_internal_error(format!("Failed to render HTML: {}", e)))?;

        build_html_response(200, html)
    }

    // ========================================================================
    // Redirects
    // ========================================================================

    async fn redirect_bugview_root(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError> {
        Response::builder()
            .status(302)
            .header("Location", "/bugview/index.html")
            .body(Body::empty())
            .map_err(|e| HttpError::for_internal_error(format!("Failed to build redirect: {}", e)))
    }
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
    // Required credentials - fail fast if not set rather than starting with invalid config
    let jira_url = std::env::var("JIRA_URL").expect("JIRA_URL environment variable is required");
    let jira_username =
        std::env::var("JIRA_USERNAME").expect("JIRA_USERNAME environment variable is required");
    let jira_password =
        std::env::var("JIRA_PASSWORD").expect("JIRA_PASSWORD environment variable is required");
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
        config,
        html: html_renderer,
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

    /// Start a test server with the given context.
    ///
    /// Returns `Some(server)` on success, or `None` if the server couldn't start
    /// (in non-CI environments). Panics in CI if startup fails.
    async fn start_test_server(ctx: ApiContext) -> Option<dropshot::HttpServer<ApiContext>> {
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>()
            .expect("api description");

        let config = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };

        let log = dropshot::ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Warn,
        }
        .to_logger("bugview-test")
        .expect("logger");

        let server = match HttpServerStarter::new(&config, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start test server in CI: {}", e);
                }
                eprintln!("SKIPPING: failed to start server: {} (set CI=1 to fail)", e);
                return None;
            }
        };

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Some(server)
    }

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
            config,
            html: HtmlRenderer::new(),
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
            config,
            html: HtmlRenderer::new(),
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
            .render_issue(&issue, &filtered, false)
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

        assert_eq!(filtered.len(), 2, "Should only keep http and https links");

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
            !filtered
                .iter()
                .any(|l| l.object.as_ref().unwrap().url.contains("javascript:")),
            "javascript: URLs should be filtered"
        );
        assert!(
            !filtered
                .iter()
                .any(|l| l.object.as_ref().unwrap().url.contains("data:")),
            "data: URLs should be filtered"
        );
        assert!(
            !filtered
                .iter()
                .any(|l| l.object.as_ref().unwrap().url.contains("file:")),
            "file: URLs should be filtered"
        );
    }

    #[tokio::test]
    async fn test_http_issue_route_with_mock_server() {
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };
        let url = format!("http://{}/bugview/issue/PROJ-1", server.local_addr());

        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.text().await.expect("body");
        assert!(body.contains("Test summary"));
    }

    #[tokio::test]
    async fn test_http_index_json_with_mock_server() {
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };
        let url = format!("http://{}/bugview/index.json", server.local_addr());

        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.text().await.expect("body");
        assert!(body.contains("\"issues\""));
        assert!(body.contains("PROJ-1") && body.contains("PROJ-2"));
    }

    #[tokio::test]
    async fn test_http_index_html_with_mock_server() {
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };
        let url = format!("http://{}/bugview/index.html", server.local_addr());

        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.text().await.expect("body");
        assert!(body.contains("Public Issues Index") || body.contains("Public Issues"));
    }

    #[tokio::test]
    async fn test_http_label_index_html_disallowed() {
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };
        let url = format!("http://{}/bugview/label/not-allowed", server.local_addr());

        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_http_redirect_bugview_root() {
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };
        let url = format!("http://{}/bugview", server.local_addr());

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
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };
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
        let Some(server) = start_test_server(non_public_test_context()).await else {
            return;
        };
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

    #[tokio::test]
    async fn test_json_api_returns_400_for_invalid_pagination_token() {
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };

        // Request with an invalid pagination token
        let url = format!(
            "http://{}/bugview/index.json?next_page_token=invalid123abc",
            server.local_addr()
        );
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "JSON API should return 400 for invalid pagination token"
        );
        let body = resp.text().await.expect("body");
        assert!(
            body.contains("Invalid or expired pagination token"),
            "Error message should indicate invalid token: {}",
            body
        );
    }

    #[tokio::test]
    async fn test_html_api_gracefully_handles_invalid_pagination_token() {
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };

        // Request with an invalid pagination token - should return 200 with first page
        let url = format!(
            "http://{}/bugview/index.html?next_page_token=invalid123abc",
            server.local_addr()
        );
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "HTML API should return 200 (fallback to first page) for invalid token"
        );
        let body = resp.text().await.expect("body");
        // Should show the normal index page content
        assert!(
            body.contains("Public Issues") || body.contains("PROJ-1"),
            "HTML should show the index page content"
        );
    }

    // Mock JIRA client that returns "Issue not found" error
    #[derive(Clone, Default)]
    struct NotFoundMockJiraClient;

    #[async_trait]
    impl JiraClientTrait for NotFoundMockJiraClient {
        async fn search_issues(
            &self,
            _labels: &[String],
            _page_token: Option<&str>,
            _sort: &str,
        ) -> anyhow::Result<SearchResponse> {
            Ok(SearchResponse {
                issues: vec![],
                is_last: Some(true),
                next_page_token: None,
            })
        }

        async fn get_issue(&self, key: &jira_api::IssueKey) -> anyhow::Result<Issue> {
            anyhow::bail!("Issue not found: {}", key)
        }

        async fn get_remote_links(&self, _issue_id: &str) -> anyhow::Result<Vec<RemoteLink>> {
            Ok(vec![])
        }
    }

    fn not_found_test_context() -> ApiContext {
        let config = Config {
            default_label: "public".to_string(),
            allowed_labels: vec!["public".to_string()],
            allowed_domains: vec![],
            public_base_url: "https://test.example.com".to_string(),
        };

        ApiContext {
            jira: Arc::new(NotFoundMockJiraClient) as Arc<dyn JiraClientTrait>,
            config,
            html: HtmlRenderer::new(),
            token_cache: TokenCache::new(),
        }
    }

    #[tokio::test]
    async fn test_issue_not_found_returns_404() {
        // Test that JIRA "Issue not found" errors are converted to HTTP 404
        let Some(server) = start_test_server(not_found_test_context()).await else {
            return;
        };
        let addr = server.local_addr();

        // Test HTML endpoint
        let url = format!("http://{}/bugview/issue/PROJ-999", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "HTML endpoint should return 404 when JIRA says issue not found"
        );

        // Test JSON endpoint
        let url = format!("http://{}/bugview/json/PROJ-999", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "JSON endpoint should return 404 when JIRA says issue not found"
        );

        // Test full JSON endpoint
        let url = format!("http://{}/bugview/fulljson/PROJ-999", addr);
        let resp = reqwest::get(&url).await.expect("request");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "Full JSON endpoint should return 404 when JIRA says issue not found"
        );
    }

    #[tokio::test]
    async fn test_malformed_issue_key_returns_400() {
        let Some(server) = start_test_server(test_context()).await else {
            return;
        };
        let addr = server.local_addr();

        // Test various malformed keys
        // Note: "123-456" is actually valid - JIRA allows numeric project prefixes
        let malformed_keys = vec![
            "NOHYPHEN", // Missing hyphen
            "-123",     // Empty project part
            "PROJ-",    // Empty issue number
            "PROJ-abc", // Non-numeric issue number
        ];

        for key in &malformed_keys {
            let url = format!("http://{}/bugview/issue/{}", addr, key);
            let resp = reqwest::get(&url).await.expect("request");
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "Malformed key '{}' should return 400",
                key
            );
        }
    }
}
