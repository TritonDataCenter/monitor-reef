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

        // Comments
        if let Some(comment_obj) = issue.fields.get("comment") {
            if let Some(comments) = comment_obj.get("comments").and_then(|c| c.as_array()) {
                if !comments.is_empty() {
                    content.push_str(&format!("<h2>Comments ({})</h2>\n", comments.len()));

                    for comment in comments {
                        let author = comment
                            .get("author")
                            .and_then(|a| a.get("displayName"))
                            .and_then(|d| d.as_str())
                            .unwrap_or("Unknown");

                        let created = comment
                            .get("created")
                            .and_then(|c| c.as_str())
                            .unwrap_or("");

                        let updated = comment
                            .get("updated")
                            .and_then(|u| u.as_str())
                            .unwrap_or("");

                        content.push_str("<div class=\"well\" style=\"margin-bottom: 15px;\">\n");
                        content.push_str(&format!(
                            "<p><strong>{}</strong> commented on {}{}:</p>\n",
                            html_escape(author),
                            html_escape(created),
                            if created != updated {
                                format!(" <em>(edited {})</em>", html_escape(updated))
                            } else {
                                String::new()
                            }
                        ));

                        // Render comment body
                        if let Some(body) = comment.get("body") {
                            if let Some(body_str) = body.as_str() {
                                // Plain text fallback
                                content.push_str(&format!("<p>{}</p>", html_escape(body_str)));
                            } else if let Some(body_obj) = body.as_object() {
                                // ADF format - convert to HTML
                                if let Some(body_content) = body_obj.get("content") {
                                    let html = adf_to_html(body_content);
                                    content.push_str(&html);
                                }
                            }
                        }

                        content.push_str("</div>\n");
                    }
                }
            }
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

/// Convert ADF (Atlassian Document Format) to HTML
///
/// This function handles the same ADF node types as the CLI text extractor,
/// but outputs HTML instead of plain text.
fn adf_to_html(nodes: &serde_json::Value) -> String {
    let mut result = String::new();

    if let Some(nodes_array) = nodes.as_array() {
        for node in nodes_array.iter() {
            if let Some(node_obj) = node.as_object() {
                let node_type = node_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match node_type {
                    "paragraph" => {
                        result.push_str("<p>");
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&adf_to_html(content));
                        }
                        result.push_str("</p>\n");
                    }

                    "text" => {
                        if let Some(text) = node_obj.get("text").and_then(|t| t.as_str()) {
                            let mut formatted_text = html_escape(text);

                            // Apply marks (formatting)
                            if let Some(marks) = node_obj.get("marks").and_then(|m| m.as_array()) {
                                for mark in marks {
                                    if let Some(mark_type) = mark.get("type").and_then(|t| t.as_str()) {
                                        formatted_text = match mark_type {
                                            "strong" => format!("<strong>{}</strong>", formatted_text),
                                            "em" => format!("<em>{}</em>", formatted_text),
                                            "code" => format!("<code>{}</code>", formatted_text),
                                            "link" => {
                                                if let Some(href) = mark.get("attrs")
                                                    .and_then(|a| a.get("href"))
                                                    .and_then(|h| h.as_str())
                                                {
                                                    format!(r#"<a href="{}" rel="noopener noreferrer" target="_blank">{}</a>"#,
                                                        html_escape(href), formatted_text)
                                                } else {
                                                    formatted_text
                                                }
                                            }
                                            _ => formatted_text,
                                        };
                                    }
                                }
                            }

                            result.push_str(&formatted_text);
                        }
                    }

                    "inlineCard" => {
                        if let Some(attrs) = node_obj.get("attrs") {
                            if let Some(url) = attrs.get("url").and_then(|u| u.as_str()) {
                                // Extract issue key from URL
                                if let Some(issue_key) = url.split('/').last() {
                                    result.push_str(&format!(
                                        r#"<a href="{}" rel="noopener noreferrer" target="_blank">{}</a>"#,
                                        html_escape(url),
                                        html_escape(issue_key)
                                    ));
                                } else {
                                    result.push_str(&format!(
                                        r#"<a href="{}" rel="noopener noreferrer" target="_blank">{}</a>"#,
                                        html_escape(url),
                                        html_escape(url)
                                    ));
                                }
                            }
                        }
                    }

                    "codeBlock" => {
                        result.push_str("<pre><code>");
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&adf_to_html(content));
                        }
                        result.push_str("</code></pre>\n");
                    }

                    "hardBreak" => {
                        result.push_str("<br>\n");
                    }

                    "mention" => {
                        if let Some(attrs) = node_obj.get("attrs") {
                            if let Some(text) = attrs.get("text").and_then(|t| t.as_str()) {
                                result.push_str(&format!("<strong>@{}</strong>", html_escape(text)));
                            } else if let Some(id) = attrs.get("id").and_then(|i| i.as_str()) {
                                result.push_str(&format!("<strong>@{}</strong>", html_escape(id)));
                            }
                        }
                    }

                    "bulletList" => {
                        result.push_str("<ul>\n");
                        if let Some(content) = node_obj.get("content") {
                            if let Some(items) = content.as_array() {
                                for item in items {
                                    result.push_str("<li>");
                                    if let Some(item_content) = item.get("content") {
                                        result.push_str(&adf_to_html(item_content));
                                    }
                                    result.push_str("</li>\n");
                                }
                            }
                        }
                        result.push_str("</ul>\n");
                    }

                    "orderedList" => {
                        result.push_str("<ol>\n");
                        if let Some(content) = node_obj.get("content") {
                            if let Some(items) = content.as_array() {
                                for item in items {
                                    result.push_str("<li>");
                                    if let Some(item_content) = item.get("content") {
                                        result.push_str(&adf_to_html(item_content));
                                    }
                                    result.push_str("</li>\n");
                                }
                            }
                        }
                        result.push_str("</ol>\n");
                    }

                    "listItem" => {
                        // Handled by parent list
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&adf_to_html(content));
                        }
                    }

                    "heading" => {
                        let level = node_obj.get("attrs")
                            .and_then(|a| a.get("level"))
                            .and_then(|l| l.as_u64())
                            .unwrap_or(1)
                            .min(6);
                        result.push_str(&format!("<h{}>", level));
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&adf_to_html(content));
                        }
                        result.push_str(&format!("</h{}>\n", level));
                    }

                    "panel" => {
                        result.push_str(r#"<div class="alert alert-info" style="margin: 10px 0;">"#);
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&adf_to_html(content));
                        }
                        result.push_str("</div>\n");
                    }

                    _ => {
                        // For unknown node types, try to extract content recursively
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&adf_to_html(content));
                        }
                    }
                }
            }
        }
    }

    result
}

/// Simple HTML escape function
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
