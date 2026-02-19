// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Environment variable export command

use crate::config::paths::config_dir;
use crate::config::{Profile, resolve_profile};
use anyhow::Result;
use std::collections::BTreeMap;

/// Read Docker environment variables from the profile's setup.json file.
///
/// Returns the env map from `config_dir/docker/<profile_name>/setup.json`
/// if the file exists and is valid, or an empty map otherwise.
fn read_docker_env(profile_name: &str) -> BTreeMap<String, String> {
    let setup_path = config_dir()
        .join("docker")
        .join(profile_name)
        .join("setup.json");
    let contents = match std::fs::read_to_string(&setup_path) {
        Ok(c) => c,
        Err(_) => return BTreeMap::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return BTreeMap::new(),
    };
    let Some(env_obj) = parsed.get("env").and_then(|v| v.as_object()) else {
        return BTreeMap::new();
    };
    // Collect non-null string values, using BTreeMap for deterministic ordering
    env_obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

/// Generate shell export statements for the profile
pub async fn generate_env(profile_name: Option<&str>, shell: &str) -> Result<()> {
    let profile = resolve_profile(profile_name).await?;
    let docker_env = read_docker_env(&profile.name);

    match shell {
        "bash" | "sh" | "zsh" => print_posix_exports(&profile, &docker_env),
        "fish" => print_fish_exports(&profile, &docker_env),
        "powershell" | "pwsh" => print_powershell_exports(&profile, &docker_env),
        _ => print_posix_exports(&profile, &docker_env),
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

fn print_posix_exports(profile: &Profile, docker_env: &BTreeMap<String, String>) {
    // triton section
    println!("# triton");
    println!(
        "export TRITON_PROFILE=\"{}\"",
        shell_escape_double(&profile.name)
    );

    // docker section
    println!("# docker");
    for (key, value) in docker_env {
        println!("export {}=\"{}\"", key, shell_escape_double(value));
    }

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

fn print_fish_exports(profile: &Profile, docker_env: &BTreeMap<String, String>) {
    // triton section
    println!("# triton");
    println!(
        "set -gx TRITON_PROFILE '{}'",
        shell_escape_single(&profile.name)
    );

    // docker section
    println!("# docker");
    for (key, value) in docker_env {
        println!("set -gx {} '{}'", key, shell_escape_single(value));
    }

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

fn print_powershell_exports(profile: &Profile, docker_env: &BTreeMap<String, String>) {
    // triton section
    println!("# triton");
    println!(
        "$env:TRITON_PROFILE = '{}'",
        shell_escape_powershell(&profile.name)
    );

    // docker section
    println!("# docker");
    for (key, value) in docker_env {
        println!("$env:{} = '{}'", key, shell_escape_powershell(value));
    }

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
