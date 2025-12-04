// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! ADF (Atlassian Document Format) rendering utilities
//!
//! This module provides a trait-based visitor pattern for rendering ADF content
//! into different output formats (HTML, plain text, etc.).

/// Trait for writing ADF content to different output formats
///
/// Implementors define how each ADF node type should be rendered in their
/// specific format (HTML, plain text, etc.).
///
/// # Security
///
/// **Implementors are responsible for proper output escaping.** The `render_adf`
/// function passes text content and URLs as-is from the ADF document without
/// modification. For security-sensitive outputs:
///
/// - **HTML**: All text content must be HTML-escaped (e.g., `<` → `&lt;`) to
///   prevent XSS attacks. URLs in links should be validated to use safe schemes
///   (http/https only).
/// - **Other formats**: Apply appropriate escaping for the target format.
///
/// The `HtmlWriter` implementation in `bugview-service` demonstrates proper
/// HTML escaping via the `html_escape()` function.
pub trait AdfWriter {
    /// Write plain text content
    fn write_text(&mut self, text: &str);

    /// Start a link (wraps other formatting)
    fn start_link(&mut self, url: &str);

    /// End a link
    fn end_link(&mut self);

    /// Start a paragraph block
    fn start_paragraph(&mut self);

    /// End a paragraph block
    fn end_paragraph(&mut self);

    /// Start a bullet (unordered) list
    fn start_bullet_list(&mut self);

    /// End a bullet (unordered) list
    fn end_bullet_list(&mut self);

    /// Start an ordered (numbered) list
    fn start_ordered_list(&mut self);

    /// End an ordered (numbered) list
    fn end_ordered_list(&mut self);

    /// Start a list item (called for each item in a list)
    fn start_list_item(&mut self, index: Option<usize>);

    /// End a list item
    fn end_list_item(&mut self);

    /// Start a heading
    fn start_heading(&mut self, level: u8);

    /// End a heading
    fn end_heading(&mut self, level: u8);

    /// Start a code block
    fn start_code_block(&mut self, language: Option<&str>);

    /// End a code block
    fn end_code_block(&mut self);

    /// Write a hard line break
    fn write_hard_break(&mut self);

    /// Start a panel (info/warning/error box)
    fn start_panel(&mut self, panel_type: Option<&str>);

    /// End a panel
    fn end_panel(&mut self, panel_type: Option<&str>);

    /// Start strong (bold) formatting
    fn start_strong(&mut self);

    /// End strong (bold) formatting
    fn end_strong(&mut self);

    /// Start emphasis (italic) formatting
    fn start_emphasis(&mut self);

    /// End emphasis (italic) formatting
    fn end_emphasis(&mut self);

    /// Start inline code formatting
    fn start_inline_code(&mut self);

    /// End inline code formatting
    fn end_inline_code(&mut self);

    /// Start strikethrough formatting
    fn start_strike(&mut self);

    /// End strikethrough formatting
    fn end_strike(&mut self);

    /// Write a mention (user reference)
    fn write_mention(&mut self, display: &str);

    /// Write an inline card (cross-reference to another issue)
    fn write_inline_card(&mut self, url: &str, display: &str);
}

/// Render ADF content using the provided writer
///
/// This function recursively traverses ADF nodes and calls appropriate
/// methods on the writer for each node type.
pub fn render_adf<W: AdfWriter>(nodes: &serde_json::Value, writer: &mut W) {
    render_adf_internal(nodes, writer, 0);
}

fn render_adf_internal<W: AdfWriter>(nodes: &serde_json::Value, writer: &mut W, _depth: usize) {
    if let Some(nodes_array) = nodes.as_array() {
        for node in nodes_array.iter() {
            if let Some(node_obj) = node.as_object() {
                let node_type = node_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match node_type {
                    "paragraph" => {
                        writer.start_paragraph();
                        if let Some(content) = node_obj.get("content") {
                            render_adf_internal(content, writer, _depth);
                        }
                        writer.end_paragraph();
                    }

                    "text" => {
                        if let Some(text) = node_obj.get("text").and_then(|t| t.as_str()) {
                            // Process marks (formatting) in order
                            let marks = node_obj
                                .get("marks")
                                .and_then(|m| m.as_array())
                                .map(|m| m.as_slice())
                                .unwrap_or(&[]);

                            // Track which marks are applied and extract link href
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

                            // Start link wrapper if present (link wraps everything else)
                            if let Some(href) = link_href {
                                writer.start_link(href);
                            }

                            // Apply formatting marks
                            if has_strong {
                                writer.start_strong();
                            }
                            if has_em {
                                writer.start_emphasis();
                            }
                            if has_code {
                                writer.start_inline_code();
                            }
                            if has_strike {
                                writer.start_strike();
                            }

                            // Write text
                            writer.write_text(text);

                            // Close formatting marks
                            if has_strike {
                                writer.end_strike();
                            }
                            if has_code {
                                writer.end_inline_code();
                            }
                            if has_em {
                                writer.end_emphasis();
                            }
                            if has_strong {
                                writer.end_strong();
                            }

                            // Close link wrapper
                            if link_href.is_some() {
                                writer.end_link();
                            }
                        }
                    }

                    "inlineCard" => {
                        if let Some(attrs) = node_obj.get("attrs")
                            && let Some(url) = attrs.get("url").and_then(|u| u.as_str())
                        {
                            // Extract issue key from URL (last path segment)
                            let display = url.rsplit('/').next().unwrap_or(url);
                            writer.write_inline_card(url, display);
                        }
                    }

                    "codeBlock" => {
                        let language = node_obj
                            .get("attrs")
                            .and_then(|a| a.get("language"))
                            .and_then(|l| l.as_str());
                        writer.start_code_block(language);
                        if let Some(content) = node_obj.get("content") {
                            render_adf_internal(content, writer, _depth);
                        }
                        writer.end_code_block();
                    }

                    "hardBreak" => {
                        writer.write_hard_break();
                    }

                    "mention" => {
                        if let Some(attrs) = node_obj.get("attrs") {
                            if let Some(text) = attrs.get("text").and_then(|t| t.as_str()) {
                                // Text may already contain @ prefix
                                let display = text.strip_prefix('@').unwrap_or(text);
                                writer.write_mention(display);
                            } else if let Some(id) = attrs.get("id").and_then(|i| i.as_str()) {
                                writer.write_mention(id);
                            }
                        }
                    }

                    "bulletList" => {
                        writer.start_bullet_list();
                        if let Some(content) = node_obj.get("content")
                            && let Some(items) = content.as_array()
                        {
                            for item in items {
                                writer.start_list_item(None);
                                if let Some(item_content) = item.get("content") {
                                    render_adf_internal(item_content, writer, _depth + 1);
                                }
                                writer.end_list_item();
                            }
                        }
                        writer.end_bullet_list();
                    }

                    "orderedList" => {
                        writer.start_ordered_list();
                        if let Some(content) = node_obj.get("content")
                            && let Some(items) = content.as_array()
                        {
                            for (i, item) in items.iter().enumerate() {
                                writer.start_list_item(Some(i + 1));
                                if let Some(item_content) = item.get("content") {
                                    render_adf_internal(item_content, writer, _depth + 1);
                                }
                                writer.end_list_item();
                            }
                        }
                        writer.end_ordered_list();
                    }

                    "listItem" => {
                        // List items are handled by their parent list
                        if let Some(content) = node_obj.get("content") {
                            render_adf_internal(content, writer, _depth);
                        }
                    }

                    "heading" => {
                        let level = node_obj
                            .get("attrs")
                            .and_then(|a| a.get("level"))
                            .and_then(|l| l.as_u64())
                            .unwrap_or(1)
                            .min(6) as u8;
                        writer.start_heading(level);
                        if let Some(content) = node_obj.get("content") {
                            render_adf_internal(content, writer, _depth);
                        }
                        writer.end_heading(level);
                    }

                    "panel" => {
                        let panel_type = node_obj
                            .get("attrs")
                            .and_then(|a| a.get("panelType"))
                            .and_then(|t| t.as_str());
                        writer.start_panel(panel_type);
                        if let Some(content) = node_obj.get("content") {
                            render_adf_internal(content, writer, _depth);
                        }
                        writer.end_panel(panel_type);
                    }

                    _ => {
                        // For unknown node types, try to extract content recursively
                        if let Some(content) = node_obj.get("content") {
                            render_adf_internal(content, writer, _depth);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test writer that collects calls for verification
    struct TestWriter {
        calls: Vec<String>,
    }

    impl TestWriter {
        fn new() -> Self {
            Self { calls: Vec::new() }
        }

        fn contains(&self, s: &str) -> bool {
            self.calls.iter().any(|call| call.contains(s))
        }
    }

    impl AdfWriter for TestWriter {
        fn write_text(&mut self, text: &str) {
            self.calls.push(format!("text({})", text));
        }

        fn start_link(&mut self, url: &str) {
            self.calls.push(format!("start_link({})", url));
        }

        fn end_link(&mut self) {
            self.calls.push("end_link".to_string());
        }

        fn start_paragraph(&mut self) {
            self.calls.push("start_paragraph".to_string());
        }

        fn end_paragraph(&mut self) {
            self.calls.push("end_paragraph".to_string());
        }

        fn start_bullet_list(&mut self) {
            self.calls.push("start_bullet_list".to_string());
        }

        fn end_bullet_list(&mut self) {
            self.calls.push("end_bullet_list".to_string());
        }

        fn start_ordered_list(&mut self) {
            self.calls.push("start_ordered_list".to_string());
        }

        fn end_ordered_list(&mut self) {
            self.calls.push("end_ordered_list".to_string());
        }

        fn start_list_item(&mut self, index: Option<usize>) {
            self.calls.push(format!("start_list_item({:?})", index));
        }

        fn end_list_item(&mut self) {
            self.calls.push("end_list_item".to_string());
        }

        fn start_heading(&mut self, level: u8) {
            self.calls.push(format!("start_heading({})", level));
        }

        fn end_heading(&mut self, level: u8) {
            self.calls.push(format!("end_heading({})", level));
        }

        fn start_code_block(&mut self, _language: Option<&str>) {
            self.calls.push("start_code_block".to_string());
        }

        fn end_code_block(&mut self) {
            self.calls.push("end_code_block".to_string());
        }

        fn write_hard_break(&mut self) {
            self.calls.push("hard_break".to_string());
        }

        fn start_panel(&mut self, _panel_type: Option<&str>) {
            self.calls.push("start_panel".to_string());
        }

        fn end_panel(&mut self, _panel_type: Option<&str>) {
            self.calls.push("end_panel".to_string());
        }

        fn start_strong(&mut self) {
            self.calls.push("start_strong".to_string());
        }

        fn end_strong(&mut self) {
            self.calls.push("end_strong".to_string());
        }

        fn start_emphasis(&mut self) {
            self.calls.push("start_emphasis".to_string());
        }

        fn end_emphasis(&mut self) {
            self.calls.push("end_emphasis".to_string());
        }

        fn start_inline_code(&mut self) {
            self.calls.push("start_inline_code".to_string());
        }

        fn end_inline_code(&mut self) {
            self.calls.push("end_inline_code".to_string());
        }

        fn start_strike(&mut self) {
            self.calls.push("start_strike".to_string());
        }

        fn end_strike(&mut self) {
            self.calls.push("end_strike".to_string());
        }

        fn write_mention(&mut self, display: &str) {
            self.calls.push(format!("mention({})", display));
        }

        fn write_inline_card(&mut self, url: &str, display: &str) {
            self.calls
                .push(format!("inline_card({}, {})", display, url));
        }
    }

    #[test]
    fn paragraph_with_text() {
        let adf = serde_json::json!([
            {"type": "paragraph", "content": [{"type": "text", "text": "Hello"}]}
        ]);
        let mut writer = TestWriter::new();
        render_adf(&adf, &mut writer);
        assert!(writer.contains("start_paragraph"));
        assert!(writer.contains("text(Hello)"));
        assert!(writer.contains("end_paragraph"));
    }

    #[test]
    fn text_with_strong_mark() {
        let adf = serde_json::json!([
            {"type": "text", "text": "bold", "marks": [{"type": "strong"}]}
        ]);
        let mut writer = TestWriter::new();
        render_adf(&adf, &mut writer);
        assert!(writer.contains("start_strong"));
        assert!(writer.contains("text(bold)"));
        assert!(writer.contains("end_strong"));
    }

    #[test]
    fn inline_card_extracts_last_segment() {
        let adf = serde_json::json!([
            {"type": "inlineCard", "attrs": {"url": "https://example.com/ABC-123"}}
        ]);
        let mut writer = TestWriter::new();
        render_adf(&adf, &mut writer);
        assert!(writer.contains("inline_card(ABC-123, https://example.com/ABC-123)"));
    }

    #[test]
    fn bullet_list_with_items() {
        let adf = serde_json::json!([
            {
                "type": "bulletList",
                "content": [
                    {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Item 1"}]}]},
                    {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Item 2"}]}]}
                ]
            }
        ]);
        let mut writer = TestWriter::new();
        render_adf(&adf, &mut writer);
        assert!(writer.contains("start_bullet_list"));
        assert!(writer.contains("start_list_item(None)"));
        assert!(writer.contains("text(Item 1)"));
        assert!(writer.contains("text(Item 2)"));
        assert!(writer.contains("end_bullet_list"));
    }

    #[test]
    fn test_render_adf_escapes_xss_in_text() {
        let input = serde_json::json!([{
            "type": "text",
            "text": "<script>alert('xss')</script>"
        }]);
        let mut writer = TestWriter::new();
        render_adf(&input, &mut writer);
        // Text is passed as-is to writer; HTML escaping happens at the HTML writer level
        // This test verifies the XSS string is preserved and passed through the writer
        assert!(writer.contains("<script>alert('xss')</script>"));
    }

    #[test]
    fn test_render_adf_handles_empty_array() {
        let input = serde_json::json!([]);
        let mut writer = TestWriter::new();
        render_adf(&input, &mut writer);
        // Should produce no calls without errors
        assert_eq!(writer.calls.len(), 0);
    }

    #[test]
    fn test_render_adf_handles_null_content() {
        let input = serde_json::json!([{
            "type": "paragraph",
            "content": null
        }]);
        let mut writer = TestWriter::new();
        render_adf(&input, &mut writer);
        // Should handle gracefully: start/end paragraph without inner content
        assert!(writer.contains("start_paragraph"));
        assert!(writer.contains("end_paragraph"));
    }

    #[test]
    fn test_render_adf_handles_missing_type() {
        let input = serde_json::json!([{
            "text": "no type field"
        }]);
        let mut writer = TestWriter::new();
        render_adf(&input, &mut writer);
        // Should not panic; unknown node type with no content falls through gracefully
        assert_eq!(writer.calls.len(), 0);
    }

    #[test]
    fn test_render_adf_handles_deeply_nested_lists() {
        let input = serde_json::json!([{
            "type": "bulletList",
            "content": [{
                "type": "listItem",
                "content": [{
                    "type": "bulletList",
                    "content": [{
                        "type": "listItem",
                        "content": [{
                            "type": "paragraph",
                            "content": [{"type": "text", "text": "deeply nested"}]
                        }]
                    }]
                }]
            }]
        }]);
        let mut writer = TestWriter::new();
        render_adf(&input, &mut writer);
        // Verify nested structure renders correctly
        assert!(writer.contains("start_bullet_list"));
        assert!(writer.contains("start_list_item(None)"));
        assert!(writer.contains("text(deeply nested)"));
        assert!(writer.contains("end_bullet_list"));
        // Count occurrences: should have 2 start_bullet_list and 2 end_bullet_list for nesting
        let start_count = writer
            .calls
            .iter()
            .filter(|c| *c == "start_bullet_list")
            .count();
        let end_count = writer
            .calls
            .iter()
            .filter(|c| *c == "end_bullet_list")
            .count();
        assert_eq!(start_count, 2);
        assert_eq!(end_count, 2);
    }
}
