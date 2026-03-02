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
/// Returns `None` if the setup.json file does not exist (docker not configured),
/// `Some(map)` if the file exists (map may be empty if no env vars are set).
/// Values are `Some(string)` for set variables and `None` for null (unset) values.
async fn read_docker_env(profile_name: &str) -> Option<BTreeMap<String, Option<String>>> {
    let setup_path = config_dir()
        .join("docker")
        .join(profile_name)
        .join("setup.json");
    let contents = match tokio::fs::read_to_string(&setup_path).await {
        Ok(c) => c,
        Err(_) => return None,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return Some(BTreeMap::new()),
    };
    let Some(env_obj) = parsed.get("env").and_then(|v| v.as_object()) else {
        return Some(BTreeMap::new());
    };
    // Collect env values, preserving nulls so callers can emit unset commands
    Some(
        env_obj
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().map(|s| s.to_string())))
            .collect(),
    )
}

/// Generate shell export statements for the profile.
///
/// If any section flag is set, only those sections are emitted.
/// If none are set, all sections are emitted.
/// When `unset` is true, emit unset commands instead of exports.
pub async fn generate_env(
    profile_name: Option<&str>,
    shell: &str,
    triton_section: bool,
    docker_section: bool,
    smartdc_section: bool,
    unset: bool,
) -> Result<()> {
    let profile = resolve_profile(profile_name).await?;
    let docker_env = read_docker_env(&profile.name).await;

    // If docker was explicitly requested but setup.json is missing, error out.
    // (In unset mode the static var list is used, so no setup.json is needed.)
    if !unset && docker_section && docker_env.is_none() {
        anyhow::bail!(
            "Could not find Docker environment setup for profile \"{}\". \
             Run 'triton profile docker-setup {}' to set up.",
            profile.name,
            profile.name
        );
    }
    let docker_env = docker_env.unwrap_or_default();

    // If no section flags specified, emit all sections
    let emit_all = !triton_section && !docker_section && !smartdc_section;
    let emit_triton = emit_all || triton_section;
    let emit_docker = emit_all || docker_section;
    let emit_smartdc = emit_all || smartdc_section;

    match shell {
        "bash" | "sh" | "zsh" => {
            if unset {
                print_posix_unsets(emit_triton, emit_docker, emit_smartdc);
            } else {
                print_posix_exports(
                    &profile,
                    &docker_env,
                    emit_triton,
                    emit_docker,
                    emit_smartdc,
                );
            }
        }
        "fish" => {
            if unset {
                print_fish_unsets(emit_triton, emit_docker, emit_smartdc);
            } else {
                print_fish_exports(
                    &profile,
                    &docker_env,
                    emit_triton,
                    emit_docker,
                    emit_smartdc,
                );
            }
        }
        "powershell" | "pwsh" => {
            if unset {
                print_powershell_unsets(emit_triton, emit_docker, emit_smartdc);
            } else {
                print_powershell_exports(
                    &profile,
                    &docker_env,
                    emit_triton,
                    emit_docker,
                    emit_smartdc,
                );
            }
        }
        _ => {
            if unset {
                print_posix_unsets(emit_triton, emit_docker, emit_smartdc);
            } else {
                print_posix_exports(
                    &profile,
                    &docker_env,
                    emit_triton,
                    emit_docker,
                    emit_smartdc,
                );
            }
        }
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

const TRITON_VARS: &[&str] = &[
    "TRITON_PROFILE",
    "TRITON_URL",
    "TRITON_ACCOUNT",
    "TRITON_USER",
    "TRITON_KEY_ID",
    "TRITON_TLS_INSECURE",
];

const DOCKER_VARS: &[&str] = &[
    "DOCKER_HOST",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
    "COMPOSE_HTTP_TIMEOUT",
];

const SMARTDC_VARS: &[&str] = &[
    "SDC_URL",
    "SDC_ACCOUNT",
    "SDC_USER",
    "SDC_KEY_ID",
    "SDC_TESTING",
];

fn print_posix_exports(
    profile: &Profile,
    docker_env: &BTreeMap<String, Option<String>>,
    emit_triton: bool,
    emit_docker: bool,
    emit_smartdc: bool,
) {
    if emit_triton {
        println!("# triton");
        println!(
            "export TRITON_PROFILE=\"{}\"",
            shell_escape_double(&profile.name)
        );
    }

    if emit_docker {
        println!("# docker");
        for (key, value) in docker_env {
            match value {
                Some(v) => println!("export {}=\"{}\"", key, shell_escape_double(v)),
                None => println!("unset {}", key),
            }
        }
    }

    if emit_smartdc {
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
        if profile.insecure {
            println!("export SDC_TESTING=\"true\"");
        } else {
            println!("unset SDC_TESTING");
        }
    }

    // Only show the eval hint when emitting all sections in export mode
    if emit_triton && emit_docker && emit_smartdc {
        println!("# Run this command to configure your shell:");
        println!("#     eval \"$(triton env)\"");
    }
}

fn print_posix_unsets(emit_triton: bool, emit_docker: bool, emit_smartdc: bool) {
    if emit_triton {
        println!("# triton");
        for var in TRITON_VARS {
            println!("unset {var}");
        }
    }
    if emit_docker {
        println!("# docker");
        for var in DOCKER_VARS {
            println!("unset {var}");
        }
    }
    if emit_smartdc {
        println!("# smartdc");
        for var in SMARTDC_VARS {
            println!("unset {var}");
        }
    }
}

fn print_fish_exports(
    profile: &Profile,
    docker_env: &BTreeMap<String, Option<String>>,
    emit_triton: bool,
    emit_docker: bool,
    emit_smartdc: bool,
) {
    if emit_triton {
        println!("# triton");
        println!(
            "set -gx TRITON_PROFILE '{}'",
            shell_escape_single(&profile.name)
        );
    }

    if emit_docker {
        println!("# docker");
        for (key, value) in docker_env {
            match value {
                Some(v) => println!("set -gx {} '{}'", key, shell_escape_single(v)),
                None => println!("set -e {}", key),
            }
        }
    }

    if emit_smartdc {
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
        if profile.insecure {
            println!("set -gx SDC_TESTING 'true'");
        } else {
            println!("set -e SDC_TESTING");
        }
    }

    if emit_triton && emit_docker && emit_smartdc {
        println!("# Run this command to configure your shell:");
        println!("#     triton env | source");
    }
}

fn print_fish_unsets(emit_triton: bool, emit_docker: bool, emit_smartdc: bool) {
    if emit_triton {
        println!("# triton");
        for var in TRITON_VARS {
            println!("set -e {var}");
        }
    }
    if emit_docker {
        println!("# docker");
        for var in DOCKER_VARS {
            println!("set -e {var}");
        }
    }
    if emit_smartdc {
        println!("# smartdc");
        for var in SMARTDC_VARS {
            println!("set -e {var}");
        }
    }
}

fn print_powershell_exports(
    profile: &Profile,
    docker_env: &BTreeMap<String, Option<String>>,
    emit_triton: bool,
    emit_docker: bool,
    emit_smartdc: bool,
) {
    if emit_triton {
        println!("# triton");
        println!(
            "$env:TRITON_PROFILE = '{}'",
            shell_escape_powershell(&profile.name)
        );
    }

    if emit_docker {
        println!("# docker");
        for (key, value) in docker_env {
            match value {
                Some(v) => println!("$env:{} = '{}'", key, shell_escape_powershell(v)),
                None => {
                    println!("Remove-Item Env:{} -ErrorAction SilentlyContinue", key)
                }
            }
        }
    }

    if emit_smartdc {
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
        if profile.insecure {
            println!("$env:SDC_TESTING = 'true'");
        } else {
            println!("Remove-Item Env:SDC_TESTING -ErrorAction SilentlyContinue");
        }
    }
}

fn print_powershell_unsets(emit_triton: bool, emit_docker: bool, emit_smartdc: bool) {
    if emit_triton {
        println!("# triton");
        for var in TRITON_VARS {
            println!("Remove-Item Env:{var} -ErrorAction SilentlyContinue");
        }
    }
    if emit_docker {
        println!("# docker");
        for var in DOCKER_VARS {
            println!("Remove-Item Env:{var} -ErrorAction SilentlyContinue");
        }
    }
    if emit_smartdc {
        println!("# smartdc");
        for var in SMARTDC_VARS {
            println!("Remove-Item Env:{var} -ErrorAction SilentlyContinue");
        }
    }
}
