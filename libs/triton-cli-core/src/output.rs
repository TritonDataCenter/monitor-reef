// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Output formatting (CLI_DESIGN.md §8).
//!
//! Human-readable table on a TTY, machine-stable JSON when piped or when
//! `-o json` is given. JSON is the supported, stable interface; table
//! layout is human-facing and not stable. Scripts should pass
//! `-o json`.

use std::io::IsTerminal;

use anyhow::{Context, Result};
use serde::Serialize;

/// Output format selector. Bound to `-o/--output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table (default on a TTY).
    Table,
    /// Table with extra columns.
    Wide,
    /// Pretty JSON (default when piped; the stable interface).
    Json,
    /// YAML.
    Yaml,
}

impl OutputFormat {
    /// Resolve the effective format: an explicit `-o` wins; otherwise
    /// Table on a TTY and Json when piped or redirected.
    pub fn resolve(explicit: Option<OutputFormat>) -> OutputFormat {
        match explicit {
            Some(f) => f,
            None if std::io::stdout().is_terminal() => OutputFormat::Table,
            None => OutputFormat::Json,
        }
    }

    /// Whether this is a structured (machine) format handled by
    /// [`emit`], as opposed to a table the caller renders itself.
    pub fn is_structured(self) -> bool {
        matches!(self, OutputFormat::Json | OutputFormat::Yaml)
    }

    /// Whether the extra-column (`wide`) table variant was requested.
    pub fn is_wide(self) -> bool {
        matches!(self, OutputFormat::Wide)
    }
}

/// Emit `value` for the structured formats. Returns `true` when it
/// handled the format (Json/Yaml) so the caller can early-return;
/// returns `false` for table formats, which the caller renders with
/// [`Table`].
///
/// Typical use:
///
/// ```ignore
/// if emit(format, &resource)? {
///     return Ok(());
/// }
/// let mut t = Table::new(&["NAME", "STATE"], no_headers);
/// // ... add rows ...
/// t.print();
/// ```
pub fn emit<T: Serialize>(format: OutputFormat, value: &T) -> Result<bool> {
    match format {
        OutputFormat::Json => {
            let s = serde_json::to_string_pretty(value).context("serialize json")?;
            println!("{s}");
            Ok(true)
        }
        OutputFormat::Yaml => {
            let s = serde_yaml::to_string(value).context("serialize yaml")?;
            print!("{s}");
            Ok(true)
        }
        OutputFormat::Table | OutputFormat::Wide => Ok(false),
    }
}

/// A borderless, script-friendly table over `comfy-table`.
pub struct Table {
    inner: comfy_table::Table,
}

impl Table {
    /// New table with the given header row. When `no_headers` is set the
    /// header is omitted (for scripts that post-process columns).
    pub fn new(headers: &[&str], no_headers: bool) -> Self {
        let mut inner = comfy_table::Table::new();
        inner.load_preset(comfy_table::presets::NOTHING);
        if !no_headers {
            inner.set_header(headers.iter().map(|h| (*h).to_string()).collect::<Vec<_>>());
        }
        Self { inner }
    }

    /// Append one row. Cells are anything convertible to `String`.
    pub fn row<I, S>(&mut self, cells: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.inner
            .add_row(cells.into_iter().map(Into::into).collect::<Vec<_>>());
        self
    }

    /// Print the table to stdout.
    pub fn print(&self) {
        println!("{}", self.inner);
    }
}
