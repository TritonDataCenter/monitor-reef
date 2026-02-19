// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Environment variable export command

use crate::config::{Profile, resolve_profile};
use anyhow::Result;

/// Generate shell export statements for the profile
pub async fn generate_env(profile_name: Option<&str>, shell: &str) -> Result<()> {
    let profile = resolve_profile(profile_name).await?;

    match shell {
        "bash" | "sh" | "zsh" => print_posix_exports(&profile),
        "fish" => print_fish_exports(&profile),
        "powershell" | "pwsh" => print_powershell_exports(&profile),
        _ => print_posix_exports(&profile),
    }

    Ok(())
}

/// Escape a value for safe embedding in a POSIX double-quoted string.
///
/// In POSIX double-quoted strings, five characters have special meaning
/// and must be escaped with a backslash: `$`, `` ` ``, `\`, `"`, and `!`.
fn shell_escape_double(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '$' | '`' | '\\' | '"' | '!' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    result
}

/// Escape a value for safe embedding in a single-quoted string (fish shell).
///
/// Single-quoted strings treat all characters literally except that a
/// single quote cannot appear inside them. The standard idiom is to end
/// the current segment, insert an escaped single quote (\'), and start
/// a new segment: `value with 'quote` -> `'value with '\''quote'`
fn shell_escape_single(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Escape a value for safe embedding in a PowerShell single-quoted string.
///
/// In PowerShell single-quoted strings, the only special character is the
/// single quote itself, which is escaped by doubling it:
///   value with 'quote  ->  'value with ''quote'
fn shell_escape_powershell(value: &str) -> String {
    value.replace('\'', "''")
}

fn print_posix_exports(profile: &Profile) {
    // triton section
    println!("# triton");
    println!(
        "export TRITON_PROFILE=\"{}\"",
        shell_escape_double(&profile.name)
    );

    // docker section (placeholder for future docker host support)
    println!("# docker");

    // smartdc/SDC section for backwards compatibility
    println!("# smartdc");
    println!("export SDC_URL=\"{}\"", shell_escape_double(&profile.url));
    println!(
        "export SDC_ACCOUNT=\"{}\"",
        shell_escape_double(&profile.account)
    );

    if let Some(user) = &profile.user {
        println!("export SDC_USER=\"{}\"", shell_escape_double(user));
    } else {
        println!("unset SDC_USER");
    }

    println!(
        "export SDC_KEY_ID=\"{}\"",
        shell_escape_double(&profile.key_id)
    );
    println!("unset SDC_TESTING");

    println!("# Run this command to configure your shell:");
    println!("#     eval \"$(triton env)\"");
}

fn print_fish_exports(profile: &Profile) {
    // triton section
    println!("# triton");
    println!(
        "set -gx TRITON_PROFILE '{}'",
        shell_escape_single(&profile.name)
    );

    // docker section (placeholder for future docker host support)
    println!("# docker");

    // smartdc/SDC section for backwards compatibility
    println!("# smartdc");
    println!("set -gx SDC_URL '{}'", shell_escape_single(&profile.url));
    println!(
        "set -gx SDC_ACCOUNT '{}'",
        shell_escape_single(&profile.account)
    );

    if let Some(user) = &profile.user {
        println!("set -gx SDC_USER '{}'", shell_escape_single(user));
    } else {
        println!("set -e SDC_USER");
    }

    println!(
        "set -gx SDC_KEY_ID '{}'",
        shell_escape_single(&profile.key_id)
    );
    println!("set -e SDC_TESTING");

    println!("# Run this command to configure your shell:");
    println!("#     triton env | source");
}

fn print_powershell_exports(profile: &Profile) {
    // triton section
    println!("# triton");
    println!(
        "$env:TRITON_PROFILE = '{}'",
        shell_escape_powershell(&profile.name)
    );

    // docker section (placeholder for future docker host support)
    println!("# docker");

    // smartdc/SDC section for backwards compatibility
    println!("# smartdc");
    println!("$env:SDC_URL = '{}'", shell_escape_powershell(&profile.url));
    println!(
        "$env:SDC_ACCOUNT = '{}'",
        shell_escape_powershell(&profile.account)
    );

    if let Some(user) = &profile.user {
        println!("$env:SDC_USER = '{}'", shell_escape_powershell(user));
    } else {
        println!("Remove-Item Env:SDC_USER -ErrorAction SilentlyContinue");
    }

    println!(
        "$env:SDC_KEY_ID = '{}'",
        shell_escape_powershell(&profile.key_id)
    );
    println!("Remove-Item Env:SDC_TESTING -ErrorAction SilentlyContinue");
}
