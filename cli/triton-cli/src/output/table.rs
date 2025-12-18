// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

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
    rows: Vec<Vec<String>>,
}

impl TableBuilder {
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            long_headers: None,
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

    pub fn add_row(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    /// Print the table with the given formatting options
    pub fn print(self, opts: &TableFormatArgs) {
        let headers = if opts.long {
            self.long_headers.as_ref().unwrap_or(&self.headers)
        } else {
            &self.headers
        };

        // Determine which columns to display
        let column_indices: Vec<usize> = if let Some(ref cols) = opts.columns {
            cols.iter()
                .filter_map(|col| headers.iter().position(|h| h.eq_ignore_ascii_case(col)))
                .collect()
        } else if opts.long {
            (0..headers.len()).collect()
        } else {
            (0..self.headers.len()).collect()
        };

        // Sort rows if requested
        let mut rows = self.rows;
        if let Some((field, descending)) = opts.parse_sort()
            && let Some(idx) = headers.iter().position(|h| h.eq_ignore_ascii_case(&field))
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

        // Set padding on all columns: no left padding, 2 spaces right (for column spacing)
        let num_cols = column_indices.len();
        for col_idx in 0..num_cols {
            if let Some(column) = table.column_mut(col_idx) {
                if col_idx == num_cols - 1 {
                    // Last column should have no right padding
                    column.set_padding((0, 0));
                } else {
                    column.set_padding((0, 2));
                }
            }
        }

        if !opts.no_header {
            let header_row: Vec<&str> = column_indices
                .iter()
                .filter_map(|&i| headers.get(i).map(|s| s.as_str()))
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

        println!("{table}");
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

    // Set padding on all columns: no left padding, 2 spaces right (for column spacing)
    for col_idx in 0..headers.len() {
        if let Some(column) = table.column_mut(col_idx) {
            column.set_padding((0, 2));
        }
    }
    // Last column should have no right padding
    if let Some(last_col) = table.column_mut(headers.len() - 1) {
        last_col.set_padding((0, 0));
    }

    table
}

/// Create a new table with headers and right-aligned columns
///
/// `right_aligned` specifies which column indices should be right-aligned
/// (matching node-triton's behavior for numeric columns)
pub fn create_table_with_alignment(headers: &[&str], right_aligned: &[usize]) -> Table {
    let mut table = Table::new();
    table.load_preset(NOTHING);
    // Remove default cell padding to match node-triton's tabula output
    table.set_content_arrangement(comfy_table::ContentArrangement::Disabled);
    table.set_header(headers);

    // Set padding on all columns
    for col_idx in 0..headers.len() {
        if let Some(column) = table.column_mut(col_idx) {
            column.set_padding((0, 2)); // No left padding, 2 right (for spacing between columns)
            if right_aligned.contains(&col_idx) {
                column.set_cell_alignment(CellAlignment::Right);
            }
        }
    }
    // Last column should have no right padding
    if let Some(last_col) = table.column_mut(headers.len() - 1) {
        last_col.set_padding((0, 0));
    }

    table
}

/// Create a new table without headers
///
/// Uses no-padding format to match node-triton's tabula output
pub fn create_table_no_header(num_columns: usize) -> Table {
    let mut table = Table::new();
    table.load_preset(NOTHING);
    table.set_content_arrangement(comfy_table::ContentArrangement::Disabled);

    // We need to add a dummy row first to be able to set column padding
    // The actual rows will be added by the caller
    // For now, just return the table - padding will be applied when rows are added
    let _ = num_columns;
    table
}

/// Format a table and print it
pub fn print_table(table: Table) {
    println!("{table}");
}
