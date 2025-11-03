//! HTML rendering for bugview

use anyhow::Result;
use bugview_api::IssueListItem;
use handlebars::Handlebars;
use serde_json::json;
use std::sync::Arc;

/// HTML template renderer
pub struct HtmlRenderer {
    handlebars: Arc<Handlebars<'static>>,
}

impl HtmlRenderer {
    /// Create a new HTML renderer with templates loaded
    pub fn new() -> Result<Self> {
        let mut handlebars = Handlebars::new();

        // Load templates from the templates directory
        let templates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");

        handlebars
            .register_template_file("primary", templates_dir.join("primary.html"))
            .map_err(|e| anyhow::anyhow!("Failed to load primary template: {}", e))?;

        handlebars
            .register_template_file("issue_index", templates_dir.join("issue_index.html"))
            .map_err(|e| anyhow::anyhow!("Failed to load issue_index template: {}", e))?;

        Ok(Self {
            handlebars: Arc::new(handlebars),
        })
    }

    /// Render the issue index page
    pub fn render_issue_index(
        &self,
        issues: &[IssueListItem],
        next_page_token: Option<String>,
        is_last: bool,
        sort: &str,
        label: Option<&str>,
        allowed_labels: &[String],
    ) -> Result<String> {
        // Build label links
        let label_links: Vec<String> = allowed_labels
            .iter()
            .map(|l| {
                if Some(l.as_str()) == label {
                    format!("<b>{}</b>", html_escape(l))
                } else {
                    format!(r#"<a href="/bugview/label/{}">{}</a>"#, l, html_escape(l))
                }
            })
            .collect();

        // Build pagination links
        let page_path = if let Some(l) = label {
            format!("/bugview/label/{}", l)
        } else {
            "/bugview/index.html".to_string()
        };

        let mut pagination = Vec::new();

        // "First Page" link always goes back to start (no token)
        pagination.push(format!(
            r#"<a href="{}?sort={}">First Page</a>"#,
            page_path, sort
        ));

        let count = issues.len();
        pagination.push(format!("Displaying {} issues", count));

        // "Next Page" link if there are more results
        if !is_last {
            if let Some(token) = next_page_token {
                // URL-encode the token
                let encoded_token = urlencoding::encode(&token);
                pagination.push(format!(
                    r#"<a href="{}?next_page_token={}&sort={}">Next Page</a>"#,
                    page_path, encoded_token, sort
                ));
            }
        }

        // Render the issue_index template
        let container = self.handlebars.render(
            "issue_index",
            &json!({
                "label_text": label,
                "label_links": label_links.join(", "),
                "pagination": pagination.join(" | "),
                "issues": issues,
            }),
        )?;

        // Wrap in primary template
        let title = if let Some(l) = label {
            format!("Public Issues: {}", l)
        } else {
            "Public Issues Index".to_string()
        };

        self.handlebars.render(
            "primary",
            &json!({
                "title": title,
                "container": container,
            }),
        )
        .map_err(|e| anyhow::anyhow!("Failed to render page: {}", e))
    }

    /// Render a single issue page
    pub fn render_issue(
        &self,
        issue: &crate::jira_client::Issue,
        remote_links: &[crate::jira_client::RemoteLink],
    ) -> Result<String> {
        // Extract key fields
        let summary = issue
            .fields
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(No summary)");

        let status = issue
            .fields
            .get("status")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");

        let resolution = issue
            .fields
            .get("resolution")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str());

        let created = issue
            .fields
            .get("created")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let updated = issue
            .fields
            .get("updated")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Try to get rendered description (HTML from JIRA), fallback to raw
        let description_html = if let Some(rendered_fields) = &issue.rendered_fields {
            rendered_fields
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        let description_raw = issue
            .fields
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Build HTML content
        let mut content = String::new();
        content.push_str(&format!("<h1>{}</h1>\n", html_escape(&issue.key)));
        content.push_str(&format!("<h2>{}</h2>\n", html_escape(summary)));

        content.push_str("<dl class=\"dl-horizontal\">\n");
        content.push_str(&format!("<dt>Status:</dt><dd>{}</dd>\n", html_escape(status)));

        if let Some(res) = resolution {
            content.push_str(&format!("<dt>Resolution:</dt><dd>{}</dd>\n", html_escape(res)));
        }

        content.push_str(&format!("<dt>Created:</dt><dd>{}</dd>\n", html_escape(created)));
        content.push_str(&format!("<dt>Updated:</dt><dd>{}</dd>\n", html_escape(updated)));
        content.push_str("</dl>\n");

        // Description - use rendered HTML if available, otherwise fallback to escaped plain text
        if let Some(rendered) = description_html {
            if !rendered.is_empty() {
                content.push_str("<h3>Description</h3>\n");
                content.push_str("<div class=\"well\">\n");
                // JIRA has already rendered this to HTML, so use it directly
                content.push_str(&rendered);
                content.push_str("</div>\n");
            }
        } else if !description_raw.is_empty() {
            content.push_str("<h3>Description</h3>\n");
            content.push_str("<div class=\"well\">\n");
            // Fallback: escape and preserve as plain text
            content.push_str(&format!("<pre>{}</pre>", html_escape(description_raw)));
            content.push_str("</div>\n");
        }

        // Remote Links (filtered by allowed_domains)
        if !remote_links.is_empty() {
            content.push_str("<h2>Related Links</h2>\n");
            content.push_str("<p><ul>\n");

            for link in remote_links {
                if let Some(obj) = &link.object {
                    content.push_str("<li>");
                    content.push_str(&format!(
                        r#"<a rel="noopener noreferrer" target="_blank" href="{}">{}</a>"#,
                        html_escape(&obj.url),
                        html_escape(&obj.title)
                    ));
                    content.push_str("</li>\n");
                }
            }

            content.push_str("</ul></p>\n");
        }

        // Wrap in primary template
        self.handlebars.render(
            "primary",
            &json!({
                "title": format!("{} - Bugview", issue.key),
                "container": content,
            }),
        )
        .map_err(|e| anyhow::anyhow!("Failed to render page: {}", e))
    }

    /// Render an error page
    pub fn render_error(&self, status_code: u16, message: &str) -> Result<String> {
        let title = match status_code {
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "Error",
        };

        let content = format!(
            r#"<div class="alert alert-danger">
    <h1>{} - {}</h1>
    <p>{}</p>
    <p><a href="/bugview/index.html">Return to issue index</a></p>
</div>"#,
            status_code,
            html_escape(title),
            html_escape(message)
        );

        self.handlebars.render(
            "primary",
            &json!({
                "title": title,
                "container": content,
            }),
        )
        .map_err(|e| anyhow::anyhow!("Failed to render error page: {}", e))
    }
}

/// Simple HTML escape function
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
