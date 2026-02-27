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
        for line in table.trim_fmt().lines() {
            println!("{}", line.trim_start());
        }
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
