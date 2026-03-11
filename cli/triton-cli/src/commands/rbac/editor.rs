// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! $EDITOR integration for editing RBAC objects in commented YAML format.
//!
//! Based on node-triton's `lib/common.js:editInEditor` implementation.

use anyhow::{Result, anyhow};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::{Command, Stdio};

/// Result of editing in $EDITOR
pub struct EditResult {
    /// The edited content
    pub content: String,
    /// Whether the content changed from the original
    pub changed: bool,
}

/// Launch $EDITOR to edit text, returns edited content and whether it changed.
///
/// Creates a temporary file with the provided text, opens it in the user's
/// preferred editor ($EDITOR, or /usr/bin/vi as fallback), and reads back
/// the result after the editor exits.
///
/// # Arguments
///
/// * `text` - The initial text content to edit
/// * `filename` - A descriptive filename suffix for the temp file
///
/// # Returns
///
/// An `EditResult` containing the edited content and whether it changed.
pub fn edit_in_editor(text: &str, filename: &str) -> Result<EditResult> {
    let tmp_dir = env::temp_dir();
    let tmp_path = tmp_dir.join(format!("triton-{}-edit-{}", std::process::id(), filename));

    fs::write(&tmp_path, text)?;

    let editor = env::var("EDITOR").unwrap_or_else(|_| "/usr/bin/vi".into());

    // Parse editor command (handles "code --wait", "vim", etc.)
    let mut parts = editor.split_whitespace();
    let program = parts.next().ok_or_else(|| anyhow!("Empty EDITOR"))?;
    let mut cmd = Command::new(program);
    for arg in parts {
        cmd.arg(arg);
    }

    let status = cmd
        .arg(&tmp_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !status.success() {
        fs::remove_file(&tmp_path).ok();
        return Err(anyhow!(
            "Editor exited with status: {}",
            status.code().unwrap_or(-1)
        ));
    }

    let after_text = fs::read_to_string(&tmp_path)?;
    fs::remove_file(&tmp_path).ok();

    Ok(EditResult {
        changed: after_text != text,
        content: after_text,
    })
}

/// Prompt user to retry editing after an error.
///
/// Returns `Ok(true)` if user presses Enter, or an error if interrupted.
pub fn prompt_retry() -> Result<bool> {
    eprint!("Press Enter to re-edit, Ctrl+C to abort: ");
    io::stderr().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(true)
}

/// Format a list for YAML output (handles empty lists and indentation).
pub fn format_yaml_list(items: &[String], indent: &str) -> String {
    if items.is_empty() {
        "[]".to_string()
    } else {
        items
            .iter()
            .map(|item| format!("{}- {}", indent, item))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
