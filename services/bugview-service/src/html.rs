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
            key: issue.key.as_str(),
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

/// HTML writer that implements AdfWriter trait
struct HtmlWriter {
    output: String,
}

impl HtmlWriter {
    fn new() -> Self {
        Self {
            output: String::new(),
        }
    }

    fn into_string(self) -> String {
        self.output
    }
}

impl bugview_api::adf::AdfWriter for HtmlWriter {
    fn write_text(&mut self, text: &str) {
        self.output.push_str(&html_escape(text));
    }

    fn start_link(&mut self, url: &str) {
        self.output.push_str(&format!(
            r#"<a href="{}" rel="noopener noreferrer" target="_blank">"#,
            html_escape(url)
        ));
    }

    fn end_link(&mut self) {
        self.output.push_str("</a>");
    }

    fn start_paragraph(&mut self) {
        self.output.push_str("<p>");
    }

    fn end_paragraph(&mut self) {
        self.output.push_str("</p>\n");
    }

    fn start_bullet_list(&mut self) {
        self.output.push_str("<ul>\n");
    }

    fn end_bullet_list(&mut self) {
        self.output.push_str("</ul>\n");
    }

    fn start_ordered_list(&mut self) {
        self.output.push_str("<ol>\n");
    }

    fn end_ordered_list(&mut self) {
        self.output.push_str("</ol>\n");
    }

    fn start_list_item(&mut self, _index: Option<usize>) {
        self.output.push_str("<li>");
    }

    fn end_list_item(&mut self) {
        self.output.push_str("</li>\n");
    }

    fn start_heading(&mut self, level: u8) {
        self.output.push_str(&format!("<h{}>", level));
    }

    fn end_heading(&mut self, level: u8) {
        self.output.push_str(&format!("</h{}>\n", level));
    }

    fn start_code_block(&mut self, _language: Option<&str>) {
        self.output.push_str("<pre><code>");
    }

    fn end_code_block(&mut self) {
        self.output.push_str("</code></pre>\n");
    }

    fn write_hard_break(&mut self) {
        self.output.push_str("<br>\n");
    }

    fn start_panel(&mut self, _panel_type: Option<&str>) {
        self.output
            .push_str(r#"<div class="alert alert-info" style="margin: 10px 0;">"#);
    }

    fn end_panel(&mut self, _panel_type: Option<&str>) {
        self.output.push_str("</div>\n");
    }

    fn start_strong(&mut self) {
        self.output.push_str("<strong>");
    }

    fn end_strong(&mut self) {
        self.output.push_str("</strong>");
    }

    fn start_emphasis(&mut self) {
        self.output.push_str("<em>");
    }

    fn end_emphasis(&mut self) {
        self.output.push_str("</em>");
    }

    fn start_inline_code(&mut self) {
        self.output.push_str("<code>");
    }

    fn end_inline_code(&mut self) {
        self.output.push_str("</code>");
    }

    fn start_strike(&mut self) {
        self.output.push_str("<del>");
    }

    fn end_strike(&mut self) {
        self.output.push_str("</del>");
    }

    fn write_mention(&mut self, display: &str) {
        self.output
            .push_str(&format!("<strong>@{}</strong>", html_escape(display)));
    }

    fn write_inline_card(&mut self, url: &str, display: &str) {
        self.output.push_str(&format!(
            r#"<a href="{}" rel="noopener noreferrer" target="_blank">{}</a>"#,
            html_escape(url),
            html_escape(display)
        ));
    }
}

/// Convert ADF (Atlassian Document Format) to HTML
///
/// This function uses the shared ADF rendering logic from bugview-api.
fn adf_to_html(nodes: &serde_json::Value) -> String {
    let mut writer = HtmlWriter::new();
    bugview_api::adf::render_adf(nodes, &mut writer);
    writer.into_string()
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
