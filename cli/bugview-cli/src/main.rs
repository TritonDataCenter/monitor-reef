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

/// Extract text from ADF (Atlassian Document Format) content for terminal display
fn extract_adf_text(nodes: &serde_json::Value) -> String {
    let mut output = String::new();
    render_adf_to_text(nodes, &mut output, 0);
    output
}

fn render_adf_to_text(nodes: &serde_json::Value, output: &mut String, indent_level: usize) {
    let Some(nodes_array) = nodes.as_array() else {
        return;
    };

    for node in nodes_array {
        let Some(node_obj) = node.as_object() else {
            continue;
        };
        let node_type = node_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match node_type {
            "paragraph" => {
                if let Some(content) = node_obj.get("content") {
                    render_adf_to_text(content, output, indent_level);
                }
                output.push('\n');
            }

            "text" => {
                if let Some(text) = node_obj.get("text").and_then(|t| t.as_str()) {
                    let marks = node_obj
                        .get("marks")
                        .and_then(|m| m.as_array())
                        .map(|m| m.as_slice())
                        .unwrap_or(&[]);

                    let mut has_strong = false;
                    let mut has_em = false;
                    let mut has_code = false;
                    let mut has_strike = false;
                    let mut link_href: Option<&str> = None;

                    for mark in marks {
                        if let Some(mark_type) = mark.get("type").and_then(|t| t.as_str()) {
                            match mark_type {
                                "strong" => has_strong = true,
                                "em" => has_em = true,
                                "code" => has_code = true,
                                "strike" => has_strike = true,
                                "link" => {
                                    link_href = mark
                                        .get("attrs")
                                        .and_then(|a| a.get("href"))
                                        .and_then(|h| h.as_str());
                                }
                                _ => {}
                            }
                        }
                    }

                    if link_href.is_some() {
                        output.push('[');
                    }
                    if has_strong {
                        output.push_str("**");
                    }
                    if has_em {
                        output.push('*');
                    }
                    if has_code {
                        output.push('`');
                    }
                    if has_strike {
                        output.push_str("~~");
                    }

                    output.push_str(text);

                    if has_strike {
                        output.push_str("~~");
                    }
                    if has_code {
                        output.push('`');
                    }
                    if has_em {
                        output.push('*');
                    }
                    if has_strong {
                        output.push_str("**");
                    }
                    if let Some(href) = link_href {
                        output.push_str("](");
                        output.push_str(href);
                        output.push(')');
                    }
                }
            }

            "inlineCard" => {
                if let Some(attrs) = node_obj.get("attrs")
                    && let Some(url) = attrs.get("url").and_then(|u| u.as_str())
                {
                    let display = url.rsplit('/').next().unwrap_or(url);
                    output.push_str(&format!("[{}]", display));
                }
            }

            "codeBlock" => {
                output.push_str("\n```\n");
                if let Some(content) = node_obj.get("content") {
                    render_adf_to_text(content, output, indent_level);
                }
                output.push_str("```\n");
            }

            "hardBreak" => {
                output.push('\n');
            }

            "mention" => {
                if let Some(attrs) = node_obj.get("attrs") {
                    let display = attrs
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(|t| t.strip_prefix('@').unwrap_or(t))
                        .or_else(|| attrs.get("id").and_then(|i| i.as_str()));
                    if let Some(d) = display {
                        output.push_str(&format!("@{}", d));
                    }
                }
            }

            "bulletList" => {
                if let Some(content) = node_obj.get("content")
                    && let Some(items) = content.as_array()
                {
                    for item in items {
                        let prefix_indent = if indent_level > 0 {
                            "  ".repeat(indent_level)
                        } else {
                            String::new()
                        };
                        output.push_str(&prefix_indent);
                        output.push_str("• ");
                        if let Some(item_content) = item.get("content") {
                            render_adf_to_text(item_content, output, indent_level + 1);
                        }
                        // Trim trailing whitespace and ensure newline
                        while output.ends_with(' ') || output.ends_with('\t') {
                            output.pop();
                        }
                        if !output.ends_with('\n') {
                            output.push('\n');
                        }
                    }
                }
            }

            "orderedList" => {
                if let Some(content) = node_obj.get("content")
                    && let Some(items) = content.as_array()
                {
                    for (i, item) in items.iter().enumerate() {
                        let prefix_indent = if indent_level > 0 {
                            "  ".repeat(indent_level)
                        } else {
                            String::new()
                        };
                        output.push_str(&prefix_indent);
                        output.push_str(&format!("{}. ", i + 1));
                        if let Some(item_content) = item.get("content") {
                            render_adf_to_text(item_content, output, indent_level + 1);
                        }
                        while output.ends_with(' ') || output.ends_with('\t') {
                            output.pop();
                        }
                        if !output.ends_with('\n') {
                            output.push('\n');
                        }
                    }
                }
            }

            "listItem" => {
                if let Some(content) = node_obj.get("content") {
                    render_adf_to_text(content, output, indent_level);
                }
            }

            "heading" => {
                let level = node_obj
                    .get("attrs")
                    .and_then(|a| a.get("level"))
                    .and_then(|l| l.as_u64())
                    .unwrap_or(1)
                    .min(6) as usize;
                output.push_str(&"#".repeat(level));
                output.push(' ');
                if let Some(content) = node_obj.get("content") {
                    render_adf_to_text(content, output, indent_level);
                }
                output.push('\n');
            }

            "panel" => {
                output.push_str("\n┌");
                output.push_str(&"─".repeat(68));
                output.push_str("┐\n");
                let panel_start = output.len();
                if let Some(content) = node_obj.get("content") {
                    render_adf_to_text(content, output, indent_level);
                }
                // Process panel content to add "│ " prefix to each line
                let panel_content = output[panel_start..].to_string();
                output.truncate(panel_start);
                for line in panel_content.lines() {
                    output.push_str("│ ");
                    output.push_str(line);
                    output.push('\n');
                }
                output.push('└');
                output.push_str(&"─".repeat(68));
                output.push_str("┘\n");
            }

            _ => {
                if let Some(content) = node_obj.get("content") {
                    render_adf_to_text(content, output, indent_level);
                }
            }
        }
    }
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
                        let text = extract_adf_text(content);
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
                            let text = extract_adf_text(content);
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
    use super::*;
    use clap::CommandFactory;

    /// Test that the CLI structure is valid and has no conflicts.
    ///
    /// This catches issues like:
    /// - Duplicate short options (e.g., two args using `-n`)
    /// - Duplicate long options
    /// - Invalid argument configurations
    #[test]
    fn verify_cli_structure() {
        Cli::command().debug_assert();
    }

    use super::extract_adf_text;

    #[test]
    fn inline_card_uses_last_path_segment() {
        let adf = serde_json::json!([
            { "type": "inlineCard", "attrs": {"url": "https://example.com/path/ISSUE-123"} }
        ]);
        let out = extract_adf_text(&adf);
        assert!(out.contains("[ISSUE-123]"), "output was: {}", out);
    }

    #[test]
    fn inline_card_without_slash_uses_full_url() {
        let adf = serde_json::json!([
            {"type": "inlineCard", "attrs": {"url": "ISSUE-456"}}
        ]);
        let out = extract_adf_text(&adf);
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
        let out = extract_adf_text(&adf);
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
        let out = extract_adf_text(&adf);
        assert_eq!(out, "[**bold link**](https://example.com)");
    }
}
