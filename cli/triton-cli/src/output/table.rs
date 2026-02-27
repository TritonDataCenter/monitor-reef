// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Table output formatting

use clap::Args;
use comfy_table::{CellAlignment, Table, presets::NOTHING};

/// Common table formatting options matching node-triton's getCliTableOptions()
#[derive(Args, Clone, Default, Debug)]
pub struct TableFormatArgs {
    /// Skip table header row
    #[arg(short = 'H', long = "no-header")]
    pub no_header: bool,

    /// Specify columns to output (comma-separated)
    #[arg(short = 'o', long = "output", value_delimiter = ',')]
    pub columns: Option<Vec<String>>,

    /// Long/wider output format
    #[arg(short = 'l', long = "long")]
    pub long: bool,

    /// Sort by field (prefix with - for descending)
    #[arg(short = 's', long = "sort-by")]
    pub sort_by: Option<String>,
}

impl TableFormatArgs {
    /// Parse sort_by to get field name and direction
    pub fn parse_sort(&self) -> Option<(String, bool)> {
        self.sort_by.as_ref().map(|s| {
            if let Some(field) = s.strip_prefix('-') {
                (field.to_string(), true) // descending
            } else {
                (s.clone(), false) // ascending
            }
        })
    }
}

/// Helper struct to build and print tables with formatting options
pub struct TableBuilder {
    headers: Vec<String>,
    long_headers: Option<Vec<String>>,
    right_aligned: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl TableBuilder {
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            long_headers: None,
            right_aligned: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Set additional columns to show in long format
    pub fn with_long_headers(mut self, headers: &[&str]) -> Self {
        let mut all_headers: Vec<String> = self.headers.iter().map(|s| s.to_string()).collect();
        all_headers.extend(headers.iter().map(|s| s.to_string()));
        self.long_headers = Some(all_headers);
        self
    }

    /// Set columns that should be right-aligned (matched by header name, case-insensitive)
    pub fn with_right_aligned(mut self, columns: &[&str]) -> Self {
        self.right_aligned = columns.iter().map(|s| s.to_lowercase()).collect();
        self
    }

    pub fn add_row(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    /// Print the table with the given formatting options
    pub fn print(self, opts: &TableFormatArgs) {
        print!("{}", self.render(opts));
    }

    /// Render the table to a String with the given formatting options
    pub fn render(self, opts: &TableFormatArgs) -> String {
        let all_headers = self.long_headers.as_ref().unwrap_or(&self.headers);
        let headers = if opts.long {
            all_headers
        } else {
            &self.headers
        };

        // Determine which columns to display
        // When -o is specified, search all known headers (including long-only ones)
        // so that any field can be selected without requiring -l
        let column_indices: Vec<usize> = if let Some(ref cols) = opts.columns {
            cols.iter()
                .filter_map(|col| all_headers.iter().position(|h| h.eq_ignore_ascii_case(col)))
                .collect()
        } else if opts.long {
            (0..headers.len()).collect()
        } else {
            (0..self.headers.len()).collect()
        };

        // Sort rows if requested (resolve field against all known headers)
        let mut rows = self.rows;
        if let Some((field, descending)) = opts.parse_sort()
            && let Some(idx) = all_headers
                .iter()
                .position(|h| h.eq_ignore_ascii_case(&field))
        {
            rows.sort_by(|a, b| {
                let a_val = a.get(idx).map(|s| s.as_str()).unwrap_or("");
                let b_val = b.get(idx).map(|s| s.as_str()).unwrap_or("");
                if descending {
                    b_val.cmp(a_val)
                } else {
                    a_val.cmp(b_val)
                }
            });
        }

        // Build the table
        let mut table = Table::new();
        table.load_preset(NOTHING);
        table.set_content_arrangement(comfy_table::ContentArrangement::Disabled);

        if !opts.no_header {
            let header_row: Vec<&str> = column_indices
                .iter()
                .filter_map(|&i| all_headers.get(i).map(|s| s.as_str()))
                .collect();
            table.set_header(header_row);
        }

        for row in &rows {
            let display_row: Vec<&str> = column_indices
                .iter()
                .filter_map(|&i| row.get(i).map(|s| s.as_str()))
                .collect();
            table.add_row(display_row);
        }

        // Set padding and alignment now that columns exist
        let num_cols = column_indices.len();
        for col_idx in 0..num_cols {
            if let Some(column) = table.column_mut(col_idx) {
                if col_idx == num_cols - 1 {
                    column.set_padding((0, 0));
                } else {
                    column.set_padding((0, 2));
                }
            }
        }
        if !self.right_aligned.is_empty() {
            for (display_idx, &header_idx) in column_indices.iter().enumerate() {
                if let Some(header_name) = all_headers.get(header_idx)
                    && self.right_aligned.contains(&header_name.to_lowercase())
                    && let Some(column) = table.column_mut(display_idx)
                {
                    column.set_cell_alignment(CellAlignment::Right);
                }
            }
        }

        // Trim leading/trailing whitespace from each line
        let mut output = String::new();
        for line in table.trim_fmt().lines() {
            output.push_str(line.trim_start());
            output.push('\n');
        }
        output
    }
}

/// Create a new table with headers
///
/// Uses no-padding format to match node-triton's tabula output
pub fn create_table(headers: &[&str]) -> Table {
    let mut table = Table::new();
    table.load_preset(NOTHING);
    table.set_content_arrangement(comfy_table::ContentArrangement::Disabled);
    table.set_header(headers);

    table
}

/// Format a table and print it
///
/// Removes leading/trailing whitespace from each line to match node-triton output
pub fn print_table(table: Table) {
    // trim_fmt() removes trailing whitespace, but NOTHING preset still has
    // a leading space from the left border placeholder. Trim each line.
    for line in table.trim_fmt().lines() {
        println!("{}", line.trim_start());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 3-row table with NAME, STATE, BRAND headers (+ long-only ID)
    /// Rows are deliberately out of alphabetical order by NAME.
    fn sample_builder() -> TableBuilder {
        let mut tbl = TableBuilder::new(&["NAME", "STATE", "BRAND"]).with_long_headers(&["ID"]);
        tbl.add_row(vec![
            "charlie".into(),
            "running".into(),
            "lx".into(),
            "ccc-id".into(),
        ]);
        tbl.add_row(vec![
            "alice".into(),
            "stopped".into(),
            "bhyve".into(),
            "aaa-id".into(),
        ]);
        tbl.add_row(vec![
            "bob".into(),
            "running".into(),
            "lx".into(),
            "bbb-id".into(),
        ]);
        tbl
    }

    fn default_opts() -> TableFormatArgs {
        TableFormatArgs::default()
    }

    #[test]
    fn test_sort_by_ascending() {
        let tbl = sample_builder();
        let opts = TableFormatArgs {
            sort_by: Some("NAME".into()),
            ..default_opts()
        };
        let output = tbl.render(&opts);
        let lines: Vec<&str> = output.lines().collect();
        // Line 0 is header, line 1 is first data row
        assert!(
            lines[1].starts_with("alice"),
            "first row should be alice, got: {}",
            lines[1]
        );
        assert!(
            lines[3].starts_with("charlie"),
            "last row should be charlie, got: {}",
            lines[3]
        );
    }

    #[test]
    fn test_sort_by_descending() {
        let tbl = sample_builder();
        let opts = TableFormatArgs {
            sort_by: Some("-NAME".into()),
            ..default_opts()
        };
        let output = tbl.render(&opts);
        let lines: Vec<&str> = output.lines().collect();
        assert!(
            lines[1].starts_with("charlie"),
            "first row should be charlie, got: {}",
            lines[1]
        );
        assert!(
            lines[3].starts_with("alice"),
            "last row should be alice, got: {}",
            lines[3]
        );
    }

    #[test]
    fn test_sort_by_none_preserves_insertion_order() {
        let tbl = sample_builder();
        let output = tbl.render(&default_opts());
        let lines: Vec<&str> = output.lines().collect();
        // Insertion order: charlie, alice, bob
        assert!(
            lines[1].starts_with("charlie"),
            "first row should be charlie, got: {}",
            lines[1]
        );
        assert!(
            lines[2].starts_with("alice"),
            "second row should be alice, got: {}",
            lines[2]
        );
        assert!(
            lines[3].starts_with("bob"),
            "third row should be bob, got: {}",
            lines[3]
        );
    }

    #[test]
    fn test_columns_selects_subset() {
        let tbl = sample_builder();
        let opts = TableFormatArgs {
            columns: Some(vec!["NAME".into(), "STATE".into()]),
            ..default_opts()
        };
        let output = tbl.render(&opts);
        let header = output.lines().next().unwrap();
        assert!(header.contains("NAME"), "header should contain NAME");
        assert!(header.contains("STATE"), "header should contain STATE");
        assert!(!header.contains("BRAND"), "header should not contain BRAND");
    }

    #[test]
    fn test_columns_can_select_long_headers() {
        let tbl = sample_builder();
        let opts = TableFormatArgs {
            columns: Some(vec!["ID".into()]),
            ..default_opts()
        };
        let output = tbl.render(&opts);
        let header = output.lines().next().unwrap();
        assert!(header.contains("ID"), "header should contain ID");
        // Should not show default columns
        assert!(!header.contains("NAME"), "header should not contain NAME");
        // Data should include the ID values
        assert!(output.contains("aaa-id"), "output should contain aaa-id");
    }

    #[test]
    fn test_long_shows_extended_columns() {
        let tbl = sample_builder();
        let opts = TableFormatArgs {
            long: true,
            ..default_opts()
        };
        let output = tbl.render(&opts);
        let header = output.lines().next().unwrap();
        assert!(header.contains("ID"), "long header should contain ID");
        assert!(header.contains("NAME"), "long header should contain NAME");
    }

    #[test]
    fn test_no_header_suppresses_header_row() {
        let tbl = sample_builder();
        let opts = TableFormatArgs {
            no_header: true,
            ..default_opts()
        };
        let output = tbl.render(&opts);
        // First line should be data, not a header
        let first_line = output.lines().next().unwrap();
        assert!(
            !first_line.contains("NAME"),
            "output should not contain header NAME"
        );
        assert!(
            first_line.starts_with("charlie"),
            "first line should be data: {}",
            first_line
        );
        // Should have exactly 3 lines (3 data rows)
        assert_eq!(output.lines().count(), 3);
    }
}
