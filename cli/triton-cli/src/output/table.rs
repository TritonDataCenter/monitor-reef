// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Table output formatting

use comfy_table::{Table, presets::NOTHING};

/// Create a new table with headers
pub fn create_table(headers: &[&str]) -> Table {
    let mut table = Table::new();
    table.load_preset(NOTHING);
    table.set_header(headers);
    table
}

/// Format a table and print it
pub fn print_table(table: Table) {
    println!("{table}");
}
