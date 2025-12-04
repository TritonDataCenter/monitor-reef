// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

use anyhow::Result;
use bugview_client::Client;
use chrono::DateTime;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "bugview")]
#[command(about = "CLI for interacting with the Bugview public issue viewer", long_about = None)]
struct Cli {
    /// Base URL of the Bugview service
    /// Default is the production deployment; for local development, use http://localhost:8080
    #[arg(long, default_value = "https://smartos.org")]
    base_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List public issues (paginated)
    List {
        /// Next page token for pagination
        #[arg(long)]
        next_page_token: Option<String>,
        /// Sort field (key, created, or updated)
        #[arg(long)]
        sort: Option<String>,
    },
    /// Get issue details (terminal-friendly view or raw JSON)
    Get {
        /// Issue key (e.g., PROJECT-123)
        key: String,
        /// Output raw JSON instead of formatted view
        #[arg(long)]
        raw: bool,
    },
    /// Fetch issue data and output as a JIRA-compatible fixture for jira-stub-server
    ///
    /// This command fetches public issue data from bugview and outputs it in the
    /// format expected by jira-stub-server fixtures. The output can be redirected
    /// to a file in the fixtures directory.
    ///
    /// Example: bugview fetch-fixture TRITON-2520 > fixtures/TRITON-2520.json
    FetchFixture {
        /// Issue key (e.g., PROJECT-123)
        key: String,
    },
}

/// Text writer that implements AdfWriter trait for terminal output
struct TextWriter {
    output: String,
    indent_level: usize,
    current_link_url: Option<String>,
}

impl TextWriter {
    fn new() -> Self {
        Self {
            output: String::new(),
            indent_level: 0,
            current_link_url: None,
        }
    }

    fn into_string(self) -> String {
        self.output
    }
}

impl bugview_api::adf::AdfWriter for TextWriter {
    fn write_text(&mut self, text: &str) {
        self.output.push_str(text);
    }

    fn start_link(&mut self, url: &str) {
        self.current_link_url = Some(url.to_string());
        self.output.push('[');
    }

    fn end_link(&mut self) {
        self.output.push_str("](");
        if let Some(url) = self.current_link_url.take() {
            self.output.push_str(&url);
        }
        self.output.push(')');
    }

    fn start_paragraph(&mut self) {
        // No special marker for paragraph start
    }

    fn end_paragraph(&mut self) {
        self.output.push('\n');
    }

    fn start_bullet_list(&mut self) {
        self.indent_level += 1;
    }

    fn end_bullet_list(&mut self) {
        self.indent_level -= 1;
    }

    fn start_ordered_list(&mut self) {
        self.indent_level += 1;
    }

    fn end_ordered_list(&mut self) {
        self.indent_level -= 1;
    }

    fn start_list_item(&mut self, index: Option<usize>) {
        // Indent is already applied by parent list
        let prefix_indent = if self.indent_level > 1 {
            "  ".repeat(self.indent_level - 1)
        } else {
            String::new()
        };
        self.output.push_str(&prefix_indent);
        if let Some(num) = index {
            self.output.push_str(&format!("{}. ", num));
        } else {
            self.output.push_str("• ");
        }
    }

    fn end_list_item(&mut self) {
        // Trim trailing whitespace from list item content
        self.output = self.output.trim_end().to_string();
        self.output.push('\n');
    }

    fn start_heading(&mut self, level: u8) {
        self.output.push_str(&"#".repeat(level as usize));
        self.output.push(' ');
    }

    fn end_heading(&mut self, _level: u8) {
        self.output.push('\n');
    }

    fn start_code_block(&mut self, _language: Option<&str>) {
        self.output.push_str("\n```\n");
    }

    fn end_code_block(&mut self) {
        self.output.push_str("```\n");
    }

    fn write_hard_break(&mut self) {
        self.output.push('\n');
    }

    fn start_panel(&mut self, _panel_type: Option<&str>) {
        self.output.push_str("\n┌");
        self.output.push_str(&"─".repeat(68));
        self.output.push_str("┐\n");
    }

    fn end_panel(&mut self, _panel_type: Option<&str>) {
        // Process panel content to add "│ " prefix to each line
        let panel_start = self.output.rfind("┐\n").unwrap_or(0) + 2;
        let panel_content = self.output[panel_start..].to_string();
        self.output.truncate(panel_start);

        for line in panel_content.lines() {
            self.output.push_str("│ ");
            self.output.push_str(line);
            self.output.push('\n');
        }

        self.output.push('└');
        self.output.push_str(&"─".repeat(68));
        self.output.push_str("┘\n");
    }

    fn start_strong(&mut self) {
        self.output.push_str("**");
    }

    fn end_strong(&mut self) {
        self.output.push_str("**");
    }

    fn start_emphasis(&mut self) {
        self.output.push('*');
    }

    fn end_emphasis(&mut self) {
        self.output.push('*');
    }

    fn start_inline_code(&mut self) {
        self.output.push('`');
    }

    fn end_inline_code(&mut self) {
        self.output.push('`');
    }

    fn start_strike(&mut self) {
        self.output.push_str("~~");
    }

    fn end_strike(&mut self) {
        self.output.push_str("~~");
    }

    fn write_mention(&mut self, display: &str) {
        self.output.push_str(&format!("@{}", display));
    }

    fn write_inline_card(&mut self, _url: &str, display: &str) {
        self.output.push_str(&format!("[{}]", display));
    }
}

/// Extract text from ADF (Atlassian Document Format) content for terminal display
///
/// This function uses the shared ADF rendering logic from bugview-api.
fn extract_adf_text(nodes: &serde_json::Value, _indent_level: usize) -> String {
    let mut writer = TextWriter::new();
    bugview_api::adf::render_adf(nodes, &mut writer);
    writer.into_string()
}
/// Format a timestamp into a human-readable format
///
/// Converts ISO 8601 timestamps like "2023-10-04T10:27:22.826-0400" into
/// "Oct 4, 2023" for better readability
fn format_timestamp(timestamp: &str) -> String {
    // JIRA uses format like "2023-10-04T10:27:22.826-0400" which requires custom parsing
    // because RFC3339 requires timezone offset to have a colon (-04:00)
    let parsed = DateTime::parse_from_str(timestamp, "%Y-%m-%dT%H:%M:%S%.3f%z");

    if let Ok(dt) = parsed {
        // Format as "Month Day, Year"
        dt.format("%b %d, %Y").to_string()
    } else {
        // If parsing fails, return the original timestamp
        timestamp.to_string()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(&cli.base_url);

    match cli.command {
        Commands::List {
            next_page_token,
            sort,
        } => {
            let mut request = client.get_issue_index_json();

            if let Some(token) = next_page_token {
                request = request.next_page_token(token);
            }

            if let Some(sort_field) = sort {
                request = request.sort(sort_field);
            }

            let response = request.send().await.map_err(|e| {
                if e.to_string().contains("404") || e.to_string().contains("Not Found") {
                    anyhow::anyhow!("No issues found. The service may be unavailable or there are no public issues.")
                } else if e.to_string().contains("connection") || e.to_string().contains("dns") {
                    anyhow::anyhow!("Failed to connect to bugview service at {}\n\nPlease check the URL and your network connection.", cli.base_url)
                } else {
                    anyhow::anyhow!("Failed to fetch issue list: {}", e)
                }
            })?;
            let data = response.into_inner();

            println!("Issues (showing {}):", data.issues.len());
            println!();

            for issue in &data.issues {
                let resolution = issue
                    .resolution
                    .as_ref()
                    .map(|r| format!(" [{}]", r))
                    .unwrap_or_default();
                println!("  {}: {}{}", issue.key, issue.summary, resolution);
            }

            println!();
            if data.is_last {
                println!("(Last page)");
            }

            if let Some(next_token) = data.next_page_token {
                println!();
                println!(
                    "Next page: bugview list --next-page-token \"{}\"",
                    next_token
                );
            }
        }

        Commands::FetchFixture { key } => {
            let response = client
                .get_issue_full_json()
                .key(key.clone())
                .send()
                .await
                .map_err(|e| {
                    if e.to_string().contains("404") || e.to_string().contains("Not Found") {
                        anyhow::anyhow!("Issue '{}' not found or not public", key)
                    } else {
                        anyhow::anyhow!("Failed to fetch issue '{}': {}", key, e)
                    }
                })?;
            let issue = response.into_inner();

            // Build JIRA-compatible fixture format (drop remotelinks)
            let fixture = serde_json::json!({
                "id": issue.id,
                "key": issue.key,
                "fields": issue.fields
            });

            // Output pretty-printed JSON
            println!("{}", serde_json::to_string_pretty(&fixture)?);
        }

        Commands::Get { key, raw } => {
            let response = client.get_issue_full_json().key(key.clone()).send().await.map_err(|e| {
                if e.to_string().contains("404") || e.to_string().contains("Not Found") {
                    anyhow::anyhow!("Issue '{}' not found\n\nThe issue may not exist, may be private, or may have been deleted.\nUse 'bugview list' to see available public issues.", key)
                } else if e.to_string().contains("connection") || e.to_string().contains("dns") {
                    anyhow::anyhow!("Failed to connect to bugview service at {}\n\nPlease check the URL and your network connection.", cli.base_url)
                } else {
                    anyhow::anyhow!("Failed to fetch issue '{}': {}", key, e)
                }
            })?;
            let issue = response.into_inner();

            if raw {
                // Raw JSON output
                println!("{}", serde_json::to_string_pretty(&issue)?);
            } else {
                // Terminal-friendly formatted output
                let empty_map = serde_json::Map::new();
                let fields = issue.fields.as_object().unwrap_or(&empty_map);

                // Extract all fields
                let summary = fields
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A");

                let created = fields
                    .get("created")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A");

                let updated = fields
                    .get("updated")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A");

                let status = fields
                    .get("status")
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A");

                let issue_type = fields
                    .get("issuetype")
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A");

                let priority = fields
                    .get("priority")
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A");

                let assignee = fields
                    .get("assignee")
                    .and_then(|v| v.get("displayName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unassigned");

                let resolution = fields
                    .get("resolution")
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str());

                // Display header
                println!("{}", "═".repeat(70));
                println!("{} - {}", issue.key, summary);
                println!("{}", "═".repeat(70));
                println!();

                // Display metadata
                println!("Status:     {}", status);
                println!("Type:       {}", issue_type);
                println!("Priority:   {}", priority);
                println!("Assignee:   {}", assignee);
                if let Some(res) = resolution {
                    println!("Resolution: {}", res);
                }
                println!("Created:    {}", format_timestamp(created));
                println!("Updated:    {}", format_timestamp(updated));
                println!();

                // Display web URL
                println!(
                    "View in browser: {}/bugview/issue/{}",
                    cli.base_url, issue.key
                );
                println!();

                // Display description
                if let Some(description) = fields.get("description") {
                    println!("{}", "─".repeat(70));
                    println!("Description:");
                    println!("{}", "─".repeat(70));
                    println!();

                    // Extract text from description
                    if let Some(desc_str) = description.as_str() {
                        println!("{}", desc_str);
                    } else if let Some(desc_obj) = description.as_object()
                        && let Some(content) = desc_obj.get("content")
                    {
                        let text = extract_adf_text(content, 0);
                        print!("{}", text);
                    }
                    println!();
                }

                // Display comments
                if let Some(comment_obj) = fields.get("comment")
                    && let Some(comments) = comment_obj.get("comments").and_then(|c| c.as_array())
                    && !comments.is_empty()
                {
                    println!("{}", "─".repeat(70));
                    println!("Comments ({}):", comments.len());
                    println!("{}", "─".repeat(70));
                    println!();

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

                        println!(
                            "Comment by {} on {}{}",
                            author,
                            format_timestamp(created),
                            if created != updated {
                                format!(" (edited {})", format_timestamp(updated))
                            } else {
                                String::new()
                            }
                        );
                        println!();

                        // Extract comment body text
                        if let Some(body) = comment.get("body")
                            && let Some(body_obj) = body.as_object()
                            && let Some(content) = body_obj.get("content")
                        {
                            let text = extract_adf_text(content, 0);
                            print!("{}", text);
                        }
                        println!();
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::extract_adf_text;

    #[test]
    fn inline_card_uses_last_path_segment() {
        let adf = serde_json::json!([
            { "type": "inlineCard", "attrs": {"url": "https://example.com/path/ISSUE-123"} }
        ]);
        let out = extract_adf_text(&adf, 0);
        assert!(out.contains("[ISSUE-123]"), "output was: {}", out);
    }

    #[test]
    fn inline_card_without_slash_uses_full_url() {
        let adf = serde_json::json!([
            {"type": "inlineCard", "attrs": {"url": "ISSUE-456"}}
        ]);
        let out = extract_adf_text(&adf, 0);
        assert!(out.contains("[ISSUE-456]"));
    }

    #[test]
    fn link_mark_renders_markdown_link() {
        let adf = serde_json::json!([
            {
                "type": "text",
                "text": "click here",
                "marks": [{"type": "link", "attrs": {"href": "https://example.com"}}]
            }
        ]);
        let out = extract_adf_text(&adf, 0);
        assert_eq!(out, "[click here](https://example.com)");
    }

    #[test]
    fn link_mark_with_other_formatting() {
        let adf = serde_json::json!([
            {
                "type": "text",
                "text": "bold link",
                "marks": [
                    {"type": "strong"},
                    {"type": "link", "attrs": {"href": "https://example.com"}}
                ]
            }
        ]);
        let out = extract_adf_text(&adf, 0);
        assert_eq!(out, "[**bold link**](https://example.com)");
    }
}
