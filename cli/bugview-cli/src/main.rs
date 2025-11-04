use anyhow::Result;
use bugview_client::Client;
use chrono::DateTime;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "bugview")]
#[command(about = "CLI for interacting with the Bugview public issue viewer", long_about = None)]
struct Cli {
    /// Base URL of the Bugview service
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
}

/// Recursively extract text from ADF (Atlassian Document Format) content
///
/// This function handles various ADF node types including:
/// - paragraph: Text paragraphs
/// - text: Plain text nodes
/// - inlineCard: Cross-references to other issues
/// - codeBlock: Code snippets
/// - hardBreak: Line breaks
/// - mention: User mentions
/// - bulletList/orderedList: Lists
/// - listItem: List items
/// - strong/em/code: Text formatting (marks)
fn extract_adf_text(nodes: &serde_json::Value, indent_level: usize) -> String {
    let mut result = String::new();
    let indent = "  ".repeat(indent_level);

    if let Some(nodes_array) = nodes.as_array() {
        for node in nodes_array.iter() {
            if let Some(node_obj) = node.as_object() {
                let node_type = node_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match node_type {
                    "paragraph" => {
                        // Extract content from paragraph
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&extract_adf_text(content, indent_level));
                        }
                        result.push('\n');
                    }

                    "text" => {
                        // Extract plain text, handling marks (formatting)
                        if let Some(text) = node_obj.get("text").and_then(|t| t.as_str()) {
                            // Check for marks (bold, italic, code, etc.)
                            let mut formatted_text = text.to_string();

                            if let Some(marks) = node_obj.get("marks").and_then(|m| m.as_array()) {
                                for mark in marks {
                                    if let Some(mark_type) =
                                        mark.get("type").and_then(|t| t.as_str())
                                    {
                                        formatted_text = match mark_type {
                                            "strong" => format!("**{}**", formatted_text),
                                            "em" => format!("*{}*", formatted_text),
                                            "code" => format!("`{}`", formatted_text),
                                            _ => formatted_text,
                                        };
                                    }
                                }
                            }

                            result.push_str(&formatted_text);
                        }
                    }

                    "inlineCard" => {
                        // Extract issue key or URL from inlineCard (cross-references)
                        if let Some(attrs) = node_obj.get("attrs") {
                            if let Some(url) = attrs.get("url").and_then(|u| u.as_str()) {
                                // Try to extract issue key from URL (e.g., TRITON-2378)
                                if let Some(issue_key) = url.split('/').next_back() {
                                    result.push_str(&format!("[{}]", issue_key));
                                } else {
                                    result.push_str(&format!("[{}]", url));
                                }
                            }
                        }
                    }

                    "codeBlock" => {
                        // Extract code from codeBlock
                        result.push_str("\n```\n");
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&extract_adf_text(content, indent_level));
                        }
                        result.push_str("```\n");
                    }

                    "hardBreak" => {
                        // Insert line break
                        result.push('\n');
                    }

                    "mention" => {
                        // Extract mention (user reference)
                        if let Some(attrs) = node_obj.get("attrs") {
                            if let Some(text) = attrs.get("text").and_then(|t| t.as_str()) {
                                result.push_str(&format!("@{}", text));
                            } else if let Some(id) = attrs.get("id").and_then(|i| i.as_str()) {
                                result.push_str(&format!("@{}", id));
                            }
                        }
                    }

                    "bulletList" => {
                        // Process unordered list
                        if let Some(content) = node_obj.get("content") {
                            if let Some(items) = content.as_array() {
                                for item in items {
                                    result.push_str(&indent);
                                    result.push_str("• ");
                                    if let Some(item_content) = item.get("content") {
                                        let item_text =
                                            extract_adf_text(item_content, indent_level + 1);
                                        result.push_str(item_text.trim_end());
                                    }
                                    result.push('\n');
                                }
                            }
                        }
                    }

                    "orderedList" => {
                        // Process ordered list
                        if let Some(content) = node_obj.get("content") {
                            if let Some(items) = content.as_array() {
                                for (i, item) in items.iter().enumerate() {
                                    result.push_str(&indent);
                                    result.push_str(&format!("{}. ", i + 1));
                                    if let Some(item_content) = item.get("content") {
                                        let item_text =
                                            extract_adf_text(item_content, indent_level + 1);
                                        result.push_str(item_text.trim_end());
                                    }
                                    result.push('\n');
                                }
                            }
                        }
                    }

                    "listItem" => {
                        // Process list item content (called by bulletList/orderedList)
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&extract_adf_text(content, indent_level));
                        }
                    }

                    "heading" => {
                        // Extract heading text
                        let level = node_obj
                            .get("attrs")
                            .and_then(|a| a.get("level"))
                            .and_then(|l| l.as_u64())
                            .unwrap_or(1);
                        result.push_str(&"#".repeat(level as usize));
                        result.push(' ');
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&extract_adf_text(content, indent_level));
                        }
                        result.push('\n');
                    }

                    "panel" => {
                        // Extract panel content (info/warning/error boxes)
                        result.push_str("\n┌");
                        result.push_str(&"─".repeat(68));
                        result.push_str("┐\n");
                        if let Some(content) = node_obj.get("content") {
                            let panel_text = extract_adf_text(content, indent_level);
                            for line in panel_text.lines() {
                                result.push_str("│ ");
                                result.push_str(line);
                                result.push('\n');
                            }
                        }
                        result.push('└');
                        result.push_str(&"─".repeat(68));
                        result.push_str("┘\n");
                    }

                    _ => {
                        // For unknown node types, try to extract content recursively
                        if let Some(content) = node_obj.get("content") {
                            result.push_str(&extract_adf_text(content, indent_level));
                        }
                    }
                }
            }
        }
    }

    result
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
            if let Some(total) = data.total {
                println!("Total available: {}", total);
            }
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
                    } else if let Some(desc_obj) = description.as_object() {
                        if let Some(content) = desc_obj.get("content") {
                            let text = extract_adf_text(content, 0);
                            print!("{}", text);
                        }
                    }
                    println!();
                }

                // Display comments
                if let Some(comment_obj) = fields.get("comment") {
                    if let Some(comments) = comment_obj.get("comments").and_then(|c| c.as_array()) {
                        if !comments.is_empty() {
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
                                if let Some(body) = comment.get("body") {
                                    if let Some(body_obj) = body.as_object() {
                                        if let Some(content) = body_obj.get("content") {
                                            let text = extract_adf_text(content, 0);
                                            print!("{}", text);
                                        }
                                    }
                                }
                                println!();
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
