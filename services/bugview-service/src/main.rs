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
use jira_client::JiraClient;
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
}

/// Thread-safe cache for mapping short IDs to JIRA pagination tokens
/// This prevents exposing JIRA's tokens (which contain the JQL query) in URLs
#[derive(Clone)]
struct TokenCache {
    cache: Arc<Mutex<HashMap<String, TokenCacheEntry>>>,
    ttl: Duration,
}

impl TokenCache {
    fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            ttl: Duration::from_secs(3600), // 1 hour TTL
        }
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
        cache.insert(
            id.clone(),
            TokenCacheEntry {
                jira_token,
                expires_at: Instant::now() + self.ttl,
            },
        );

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
    jira: Arc<JiraClient>,
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
        let remote_links = ctx
            .jira
            .get_remote_links(&issue.id)
            .await
            .unwrap_or_default(); // If it fails, just show no links

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
        jira: Arc::new(jira_client),
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
