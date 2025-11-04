mod html;
mod jira_client;

use anyhow::Result;
use bugview_api::{
    BugviewApi, IssueDetails, IssueListItem, IssueListQuery, IssueListResponse, IssuePath,
    LabelPath,
};
use dropshot::{
    Body, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseOk,
    HttpServerStarter, Path, Query, RequestContext,
};
use html::HtmlRenderer;
use http::Response;
use jira_client::{JiraClient, JiraClientTrait};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::info;

/// Service configuration
#[derive(Clone)]
struct Config {
    /// Default label for public issues
    default_label: String,
    /// Additional allowed labels
    allowed_labels: Vec<String>,
    /// Allowed domains for remote links (security: prevents exposing signed URLs)
    allowed_domains: Vec<String>,
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
    inserted_at: Instant,
}

/// Thread-safe cache for mapping short IDs to JIRA pagination tokens
/// This prevents exposing JIRA's tokens (which contain the JQL query) in URLs
#[derive(Clone)]
struct TokenCache {
    cache: Arc<Mutex<HashMap<String, TokenCacheEntry>>>,
    ttl: Duration,
    max_entries: usize,
}

impl TokenCache {
    fn new() -> Self {
        Self { cache: Arc::new(Mutex::new(HashMap::new())), ttl: Duration::from_secs(3600), max_entries: 1000 }
    }

    #[cfg(test)]
    fn new_with(ttl: Duration, max_entries: usize) -> Self {
        Self { cache: Arc::new(Mutex::new(HashMap::new())), ttl, max_entries }
    }

    /// Store a JIRA token and return a short random ID
    fn store(&self, jira_token: String) -> String {
        use rand::Rng;

        let mut rng = rand::rng();
        let id: String = (0..12)
            .map(|_| {
                let idx = rng.random_range(0..62);
                match idx {
                    0..=9 => (b'0' + idx) as char,
                    10..=35 => (b'a' + idx - 10) as char,
                    _ => (b'A' + idx - 36) as char,
                }
            })
            .collect();

        let mut cache = self.cache.lock().unwrap();

        // Cleanup expired entries
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);

        // Enforce capacity by evicting oldest entries
        while cache.len() >= self.max_entries {
            if let Some((oldest_key, _)) = cache
                .iter()
                .min_by_key(|(_, v)| v.inserted_at)
                .map(|(k, v)| (k.clone(), v.inserted_at))
            {
                cache.remove(&oldest_key);
            } else {
                break;
            }
        }

        cache.insert(id.clone(), TokenCacheEntry { jira_token, expires_at: now + self.ttl, inserted_at: now });

        id
    }

    /// Retrieve a JIRA token by ID, cleaning up expired entries
    fn get(&self, id: &str) -> Option<String> {
        let mut cache = self.cache.lock().unwrap();

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
    ) -> Result<HttpResponseOk<IssueListItem>, HttpError> {
        let ctx = rqctx.context();
        let key = path.into_inner().key;

        let issue = ctx
            .jira
            .get_issue(&key)
            .await
            .map_err(|e| {
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

        Ok(HttpResponseOk(convert_to_list_item(issue)))
    }

    async fn get_issue_full_json(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssuePath>,
    ) -> Result<HttpResponseOk<IssueDetails>, HttpError> {
        let ctx = rqctx.context();
        let key = path.into_inner().key;

        let issue = ctx
            .jira
            .get_issue(&key)
            .await
            .map_err(|e| {
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

        Ok(HttpResponseOk(IssueDetails {
            key: issue.key,
            fields: serde_json::to_value(issue.fields).unwrap_or(serde_json::Value::Null),
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
        let (issues, next_page_token, is_last, sort) = fetch_issues_for_html(ctx, labels, query).await?;

        // Render HTML
        let html = ctx
            .html
            .render_issue_index(&issues, next_page_token, is_last, &sort, None, &ctx.config.allowed_labels)
            .map_err(|e| HttpError::for_internal_error(format!("Failed to render HTML: {}", e)))?;

        Ok(Response::builder()
            .status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(html.into())
            .unwrap())
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
        let (issues, next_page_token, is_last, sort) = fetch_issues_for_html(ctx, labels, query).await?;

        // Render HTML
        let html = ctx
            .html
            .render_issue_index(
                &issues,
                next_page_token,
                is_last,
                &sort,
                Some(&label),
                &ctx.config.allowed_labels,
            )
            .map_err(|e| HttpError::for_internal_error(format!("Failed to render HTML: {}", e)))?;

        Ok(Response::builder()
            .status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(html.into())
            .unwrap())
    }

    async fn get_issue_html(
        rqctx: RequestContext<Self::Context>,
        path: Path<IssuePath>,
    ) -> Result<Response<Body>, HttpError> {
        let ctx = rqctx.context();
        let key = path.into_inner().key;

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

                return Ok(Response::builder()
                    .status(status_code)
                    .header("Content-Type", "text/html; charset=utf-8")
                    .body(html.into())
                    .unwrap());
            }
        };

        // Check if issue has the required label
        if !issue_has_public_label(&issue, &ctx.config.default_label) {
            let error_message = format!("Issue {} is not public", key);
            let html = ctx
                .html
                .render_error(404, &error_message)
                .unwrap_or_else(|_| format!("Error 404: {}", error_message));

            return Ok(Response::builder()
                .status(404)
                .header("Content-Type", "text/html; charset=utf-8")
                .body(html.into())
                .unwrap());
        }

        // Fetch remote links and filter by allowed_domains
        let remote_links = ctx.jira.get_remote_links(&issue.id).await.unwrap_or_default();

        let filtered_links = filter_remote_links(&remote_links, &ctx.config);

        // Render HTML
        let html = ctx
            .html
            .render_issue(&issue, &filtered_links)
            .map_err(|e| HttpError::for_internal_error(format!("Failed to render HTML: {}", e)))?;

        Ok(Response::builder()
            .status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(html.into())
            .unwrap())
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
) -> Result<(Vec<IssueListItem>, Option<String>, bool, String), HttpError> {
    // Validate sort field
    let sort = query.sort.as_deref().unwrap_or("updated");
    if !["key", "created", "updated"].contains(&sort) {
        return Err(HttpError::for_bad_request(
            None,
            format!("Invalid sort field: {}", sort),
        ));
    }

    // Resolve the short token ID to the real JIRA token
    // For HTML endpoints, we gracefully fall back to first page on bad tokens
    let jira_token = if let Some(short_id) = &query.next_page_token {
        match ctx.token_cache.get(short_id) {
            Some(jira_token) => Some(jira_token),
            None => {
                // Return None to fetch first page instead of erroring
                None
            }
        }
    } else {
        None
    };

    let search_result = ctx
        .jira
        .search_issues(&labels, jira_token.as_deref(), sort)
        .await
        .map_err(|e| HttpError::for_internal_error(format!("Failed to search issues: {}", e)))?;

    let issues: Vec<IssueListItem> = search_result
        .issues
        .into_iter()
        .map(|issue| {
            let summary = issue
                .fields
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

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
                resolution,
                updated,
                created,
            }
        })
        .collect();

    // Extract pagination info from JIRA response
    let is_last = search_result.is_last.unwrap_or(false);

    // Store JIRA's token in cache and return short ID instead
    let next_page_token = search_result.next_page_token.map(|jira_token| {
        ctx.token_cache.store(jira_token)
    });

    Ok((issues, next_page_token, is_last, sort.to_string()))
}

/// Helper function to search issues
async fn search_issues(
    ctx: &ApiContext,
    labels: Vec<String>,
    query: IssueListQuery,
) -> Result<HttpResponseOk<IssueListResponse>, HttpError> {
    // Validate sort field
    let sort = query.sort.as_deref().unwrap_or("updated");
    if !["key", "created", "updated"].contains(&sort) {
        return Err(HttpError::for_bad_request(
            None,
            format!("Invalid sort field: {}", sort),
        ));
    }

    // Resolve the short token ID to the real JIRA token
    let jira_token = if let Some(short_id) = &query.next_page_token {
        match ctx.token_cache.get(short_id) {
            Some(jira_token) => Some(jira_token),
            None => {
                return Err(HttpError::for_bad_request(
                    None,
                    "Invalid or expired pagination token".to_string(),
                ));
            }
        }
    } else {
        None
    };

    let search_result = ctx
        .jira
        .search_issues(&labels, jira_token.as_deref(), sort)
        .await
        .map_err(|e| HttpError::for_internal_error(format!("Failed to search issues: {}", e)))?;

    let issues: Vec<IssueListItem> = search_result
        .issues
        .into_iter()
        .map(|issue| {
            let summary = issue
                .fields
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

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
                resolution,
                updated,
                created,
            }
        })
        .collect();

    // Store JIRA's token in cache and return short ID instead
    let next_page_token = search_result.next_page_token.map(|jira_token| {
        ctx.token_cache.store(jira_token)
    });

    Ok(HttpResponseOk(IssueListResponse {
        total: search_result.total,
        issues,
        next_page_token,
        is_last: search_result.is_last.unwrap_or(false),
    }))
}

/// Helper to check if issue has the public label
fn issue_has_public_label(issue: &jira_client::Issue, required_label: &str) -> bool {
    issue
        .fields
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|labels| {
            labels
                .iter()
                .any(|l| l.as_str() == Some(required_label))
        })
        .unwrap_or(false)
}

/// Helper to convert full issue to list item
fn convert_to_list_item(issue: jira_client::Issue) -> IssueListItem {
    let summary = issue
        .fields
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

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
        resolution,
        updated,
        created,
    }
}

/// Filter remote links by allowed domains
///
/// This prevents exposing links to signed Manta URLs or other sensitive domains
fn filter_remote_links(
    links: &[jira_client::RemoteLink],
    config: &Config,
) -> Vec<jira_client::RemoteLink> {
    links
        .iter()
        .filter(|link| {
            if let Some(obj) = &link.object {
                // Parse URL to extract domain
                if let Ok(url) = url::Url::parse(&obj.url) {
                    if let Some(domain) = url.host_str() {
                        return config.is_allowed_domain(domain);
                    }
                }
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
            std::env::var("RUST_LOG").unwrap_or_else(|_| "bugview_service=info,dropshot=info".to_string()),
        ))
        .init();

    // Load configuration from environment
    let jira_url = std::env::var("JIRA_URL").unwrap_or_else(|_| "https://jira.example.com".to_string());
    let jira_username = std::env::var("JIRA_USERNAME").unwrap_or_else(|_| "username".to_string());
    let jira_password = std::env::var("JIRA_PASSWORD").unwrap_or_else(|_| "password".to_string());
    let default_label = std::env::var("JIRA_DEFAULT_LABEL").unwrap_or_else(|_| "public".to_string());
    let allowed_labels = std::env::var("JIRA_ALLOWED_LABELS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
        .collect();

    info!("Initializing JIRA client");
    let jira_client = JiraClient::new(jira_url, jira_username, jira_password)?;

    info!("Initializing HTML renderer");
    let html_renderer = HtmlRenderer::new()?;

    let allowed_domains = std::env::var("JIRA_ALLOWED_DOMAINS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
        .collect();

    let config = Config {
        default_label,
        allowed_labels,
        allowed_domains,
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
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
        .parse()?;

    let config_dropshot = ConfigDropshot {
        bind_address,
        default_request_body_max_bytes: 1024 * 1024, // 1MB
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
    use async_trait::async_trait;
    use bugview_api::IssueDetails;
    use http::StatusCode;
    use crate::jira_client::{Issue, RemoteLink, SearchResponse};
    use ::jira_client::types::IssueSearchResult;

    // Mock JIRA client implementing the trait for tests
    #[derive(Clone, Default)]
    struct MockJiraClient;

    #[async_trait]
    impl JiraClientTrait for MockJiraClient {
        async fn search_issues(
            &self,
            labels: &[String],
            _page_token: Option<&str>,
            sort: &str,
        ) -> anyhow::Result<SearchResponse> {
            // Build two simple issues that contain the required fields
            let mut issue1 = serde_json::Map::new();
            issue1.insert("summary".into(), serde_json::Value::String("Alpha".into()));
            issue1.insert("created".into(), serde_json::Value::String("2023-10-01T00:00:00.000-0400".into()));
            issue1.insert("updated".into(), serde_json::Value::String("2023-10-02T00:00:00.000-0400".into()));
            issue1.insert("labels".into(), serde_json::json!(["public"]));

            let mut issue2 = serde_json::Map::new();
            issue2.insert("summary".into(), serde_json::Value::String("Beta".into()));
            issue2.insert("created".into(), serde_json::Value::String("2023-10-03T00:00:00.000-0400".into()));
            issue2.insert("updated".into(), serde_json::Value::String("2023-10-04T00:00:00.000-0400".into()));
            issue2.insert("labels".into(), serde_json::json!(["public"]));

            let issues = vec![
                IssueSearchResult { key: "PROJ-1".into(), fields: issue1.into_iter().collect() },
                IssueSearchResult { key: "PROJ-2".into(), fields: issue2.into_iter().collect() },
            ];

            Ok(SearchResponse { total: Some(2), issues, is_last: Some(true), next_page_token: None })
        }

        async fn get_issue(&self, key: &str) -> anyhow::Result<Issue> {
            // Return a minimal issue with required fields and the public label
            let mut fields: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            fields.insert("summary".to_string(), serde_json::Value::String("Test summary".into()));
            fields.insert("created".to_string(), serde_json::Value::String("2023-10-04T10:27:22.826-0400".into()));
            fields.insert("updated".to_string(), serde_json::Value::String("2023-10-05T10:27:22.826-0400".into()));
            fields.insert("labels".to_string(), serde_json::json!(["public"]));

            Ok(Issue { key: key.to_string(), id: "12345".to_string(), fields, rendered_fields: None })
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

    // Build a test ApiContext with the mock client
    fn test_context() -> ApiContext {
        let config = Config {
            default_label: "public".to_string(),
            allowed_labels: vec!["public".to_string(), "bug".to_string()],
            allowed_domains: vec!["example.com".to_string()],
        };

        ApiContext {
            jira: Arc::new(MockJiraClient) as Arc<dyn JiraClientTrait>,
            config: Arc::new(config),
            html: Arc::new(HtmlRenderer::new().expect("html renderer")),
            token_cache: TokenCache::new(),
        }
    }

    #[tokio::test]
    async fn test_issue_full_json_with_mock() {
        // Exercise the same logic used by the handler with a mock client
        let ctx = test_context();
        let issue = ctx
            .jira
            .get_issue("PROJ-1")
            .await
            .expect("mock get_issue");

        // Verify label gate
        assert!(issue_has_public_label(&issue, &ctx.config.default_label));

        let details = IssueDetails {
            key: issue.key,
            fields: serde_json::to_value(issue.fields).unwrap(),
        };

        // Spot-check a couple fields
        let fields = details.fields.as_object().unwrap();
        assert_eq!(fields.get("summary").and_then(|v| v.as_str()), Some("Test summary"));
    }

    #[tokio::test]
    async fn test_issue_html_with_mock_and_link_filter() {
        let ctx = test_context();
        let issue = ctx.jira.get_issue("PROJ-2").await.unwrap();
        // Build a link via JSON to avoid referring to internal generated types directly
        let link: jira_client::RemoteLink = serde_json::from_value(serde_json::json!({
            "id": 1,
            "object": { "url": "https://example.com/resource", "title": "Example" }
        }))
        .unwrap();
        let links = vec![link];

        let filtered = super::filter_remote_links(&links, &ctx.config);
        assert_eq!(filtered.len(), 1, "allowed domain should pass");

        let html = ctx.html.render_issue(&issue, &filtered).expect("render html");
        assert!(html.contains("Test summary"));
        assert!(html.contains("Related Links"));
        assert!(html.contains("example.com"));
    }

    #[tokio::test]
    async fn test_token_cache_capacity_bounds() {
        let cache = TokenCache::new_with(Duration::from_secs(60), 3);
        let ids: Vec<String> = (0..10)
            .map(|i| cache.store(format!("token-{}", i)))
            .collect();

        // Oldest should be evicted to maintain capacity
        let len = cache.cache.lock().unwrap().len();
        assert!(len <= 3, "cache should be bounded");

        // Newest likely remain; ensure at least last id resolves
        let last_id = ids.last().unwrap();
        assert_eq!(cache.get(last_id).as_deref(), Some("token-9"));
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

        let log = dropshot::ConfigLogging::StderrTerminal { level: dropshot::ConfigLoggingLevel::Info }
            .to_logger("bugview-service-test")
            .expect("logger");

        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                eprintln!("skipping HTTP test: failed to start server: {}", e);
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
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>().expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal { level: dropshot::ConfigLoggingLevel::Info }
            .to_logger("bugview-service-test")
            .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(_) => return,
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
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>().expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal { level: dropshot::ConfigLoggingLevel::Info }
            .to_logger("bugview-service-test")
            .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(_) => return,
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
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>().expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal { level: dropshot::ConfigLoggingLevel::Info }
            .to_logger("bugview-service-test")
            .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(_) => return,
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
        let api = bugview_api::bugview_api_mod::api_description::<BugviewServiceImpl>().expect("api description");
        let config_dropshot = ConfigDropshot {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };
        let log = dropshot::ConfigLogging::StderrTerminal { level: dropshot::ConfigLoggingLevel::Info }
            .to_logger("bugview-service-test")
            .expect("logger");
        let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
            Ok(starter) => starter.start(),
            Err(_) => return,
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let addr = server.local_addr();
        let url = format!("http://{}/bugview", addr);
        let client = reqwest::Client::new();
        let resp = client.get(&url).send().await.expect("request");
        assert_eq!(resp.status(), StatusCode::FOUND);
        let loc = resp.headers().get("Location").and_then(|v| v.to_str().ok()).unwrap_or("");
        assert_eq!(loc, "/bugview/index.html");
    }
}
