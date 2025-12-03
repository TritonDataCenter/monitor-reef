// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! HTML rendering for bugview
//!
//! This module renders JIRA issue content by converting Atlassian Document Format (ADF)
//! to HTML. ADF is the structured JSON format JIRA uses in `fields.description` and
//! comment bodies.

use anyhow::Result;
use askama::Template;
use bugview_api::{IssueListItem, IssueSort};

/// Primary layout template wrapping all pages
#[derive(Template)]
#[template(path = "primary.html")]
struct PrimaryTemplate<'a> {
    title: &'a str,
    container: &'a str,
}

/// Issue index page template
#[derive(Template)]
#[template(path = "issue_index.html")]
struct IssueIndexTemplate<'a> {
    current_label: Option<&'a str>,
    allowed_labels: &'a [String],
    page_path: &'a str,
    sort: IssueSort,
    next_page_token: Option<&'a str>,
    is_last: bool,
    issues: &'a [IssueListItem],
}

/// Single issue page template
#[derive(Template)]
#[template(path = "issue.html")]
struct IssueTemplate<'a> {
    key: &'a str,
    summary: &'a str,
    status: &'a str,
    resolution: Option<&'a str>,
    created: &'a str,
    updated: &'a str,
    description: &'a str,
    comments: &'a [CommentView],
    remote_links: &'a [RemoteLinkView],
}

/// Comment data for template rendering
pub struct CommentView {
    pub author: String,
    pub created: String,
    pub edited: Option<String>,
    pub body: String,
}

/// Remote link data for template rendering
pub struct RemoteLinkView {
    pub url: String,
    pub title: String,
}

/// HTML template renderer
#[derive(Default)]
pub struct HtmlRenderer;

impl HtmlRenderer {
    /// Create a new HTML renderer
    pub fn new() -> Self {
        Self
    }

    /// Render the issue index page
    pub fn render_issue_index(
        &self,
        issues: &[IssueListItem],
        next_page_token: Option<String>,
        is_last: bool,
        sort: IssueSort,
        label: Option<&str>,
        allowed_labels: &[String],
    ) -> Result<String> {
        // Build page path for pagination links
        let page_path = if let Some(l) = label {
            let enc = urlencoding::encode(l);
            format!("/bugview/label/{}", enc)
        } else {
            "/bugview/index.html".to_string()
        };

        // Render the issue_index template
        let index_template = IssueIndexTemplate {
            current_label: label,
            allowed_labels,
            page_path: &page_path,
            sort,
            next_page_token: next_page_token.as_deref(),
            is_last,
            issues,
        };
        let container = index_template.render()?;

        // Wrap in primary template
        let title = if let Some(l) = label {
            format!("Public Issues: {}", l)
        } else {
            "Public Issues Index".to_string()
        };

        let primary = PrimaryTemplate {
            title: &title,
            container: &container,
        };
        primary
            .render()
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

        // Render description from ADF
        let description = issue
            .fields
            .get("description")
            .and_then(|adf| adf.get("content"))
            .map(adf_to_html)
            .unwrap_or_default();

        // Extract and render comments
        let comments: Vec<CommentView> = issue
            .fields
            .get("comment")
            .and_then(|c| c.get("comments"))
            .and_then(|c| c.as_array())
            .map(|comments| {
                comments
                    .iter()
                    .map(|comment| {
                        let author = comment
                            .get("author")
                            .and_then(|a| a.get("displayName"))
                            .and_then(|d| d.as_str())
                            .unwrap_or("Unknown")
                            .to_string();

                        let created = comment
                            .get("created")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();

                        let updated = comment
                            .get("updated")
                            .and_then(|u| u.as_str())
                            .unwrap_or("");

                        let edited = if created != updated {
                            Some(updated.to_string())
                        } else {
                            None
                        };

                        let body = comment
                            .get("body")
                            .map(|body| {
                                if let Some(body_str) = body.as_str() {
                                    // Plain text fallback
                                    format!("<p>{}</p>", html_escape(body_str))
                                } else if let Some(body_content) = body.get("content") {
                                    // ADF format
                                    adf_to_html(body_content)
                                } else {
                                    String::new()
                                }
                            })
                            .unwrap_or_default();

                        CommentView {
                            author,
                            created,
                            edited,
                            body,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Extract remote links
        let link_views: Vec<RemoteLinkView> = remote_links
            .iter()
            .filter_map(|link| {
                link.object.as_ref().map(|obj| RemoteLinkView {
                    url: obj.url.clone(),
                    title: obj.title.clone(),
                })
            })
            .collect();

        // Render issue template
        let issue_template = IssueTemplate {
            key: &issue.key,
            summary,
            status,
            resolution,
            created,
            updated,
            description: &description,
            comments: &comments,
            remote_links: &link_views,
        };
        let container = issue_template.render()?;

        // Wrap in primary template
        let title = format!("{} - Bugview", issue.key);
        let primary = PrimaryTemplate {
            title: &title,
            container: &container,
        };
        primary
            .render()
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

        let primary = PrimaryTemplate {
            title,
            container: &content,
        };
        primary
            .render()
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
                                    if let Some(mark_type) =
                                        mark.get("type").and_then(|t| t.as_str())
                                    {
                                        formatted_text = match mark_type {
                                            "strong" => {
                                                format!("<strong>{}</strong>", formatted_text)
                                            }
                                            "em" => format!("<em>{}</em>", formatted_text),
                                            "code" => format!("<code>{}</code>", formatted_text),
                                            "link" => {
                                                if let Some(href) = mark
                                                    .get("attrs")
                                                    .and_then(|a| a.get("href"))
                                                    .and_then(|h| h.as_str())
                                                {
                                                    format!(
                                                        r#"<a href="{}" rel="noopener noreferrer" target="_blank">{}</a>"#,
                                                        html_escape(href),
                                                        formatted_text
                                                    )
                                                } else {
                                                    formatted_text
                                                }
                                            }
                                            "strike" => format!("<del>{}</del>", formatted_text),
                                            _ => formatted_text,
                                        };
                                    }
                                }
                            }

                            result.push_str(&formatted_text);
                        }
                    }

                    "inlineCard" => {
                        if let Some(attrs) = node_obj.get("attrs")
                            && let Some(url) = attrs.get("url").and_then(|u| u.as_str())
                        {
                            // Extract issue key from URL
                            if let Some(issue_key) = url.rsplit('/').next() {
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
                                // Text may already contain @ prefix, so strip it if present
                                let display = text.strip_prefix('@').unwrap_or(text);
                                result.push_str(&format!(
                                    "<strong>@{}</strong>",
                                    html_escape(display)
                                ));
                            } else if let Some(id) = attrs.get("id").and_then(|i| i.as_str()) {
                                result.push_str(&format!("<strong>@{}</strong>", html_escape(id)));
                            }
                        }
                    }

                    "bulletList" => {
                        result.push_str("<ul>\n");
                        if let Some(content) = node_obj.get("content")
                            && let Some(items) = content.as_array()
                        {
                            for item in items {
                                result.push_str("<li>");
                                if let Some(item_content) = item.get("content") {
                                    result.push_str(&adf_to_html(item_content));
                                }
                                result.push_str("</li>\n");
                            }
                        }
                        result.push_str("</ul>\n");
                    }

                    "orderedList" => {
                        result.push_str("<ol>\n");
                        if let Some(content) = node_obj.get("content")
                            && let Some(items) = content.as_array()
                        {
                            for item in items {
                                result.push_str("<li>");
                                if let Some(item_content) = item.get("content") {
                                    result.push_str(&adf_to_html(item_content));
                                }
                                result.push_str("</li>\n");
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
                        let level = node_obj
                            .get("attrs")
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
                        result
                            .push_str(r#"<div class="alert alert-info" style="margin: 10px 0;">"#);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_links_are_url_encoded() {
        let renderer = HtmlRenderer::new();
        let issues: Vec<IssueListItem> = vec![];
        let html = renderer
            .render_issue_index(
                &issues,
                None,
                true,
                IssueSort::Updated,
                None,
                &["needs triage".to_string()],
            )
            .expect("render");
        assert!(html.contains("/bugview/label/needs%20triage"));
    }

    #[test]
    fn pagination_path_for_label_is_encoded() {
        let renderer = HtmlRenderer::new();
        let issues: Vec<IssueListItem> = vec![];
        let html = renderer
            .render_issue_index(
                &issues,
                None,
                true,
                IssueSort::Updated,
                Some("needs triage"),
                &["needs triage".to_string()],
            )
            .expect("render");
        assert!(html.contains("/bugview/label/needs%20triage?sort=updated"));
    }

    #[test]
    fn adf_inline_card_renders_anchor_with_last_segment() {
        let input = serde_json::json!([
            {"type": "inlineCard", "attrs": {"url": "https://example.com/ABC-123"}}
        ]);
        let html = super::adf_to_html(&input);
        assert!(
            html.contains("href=\"https://example.com/ABC-123\""),
            "html: {}",
            html
        );
        assert!(html.contains(">ABC-123<"));
    }
}
