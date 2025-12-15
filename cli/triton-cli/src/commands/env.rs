// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Environment variable export command

use crate::config::{Profile, resolve_profile};
use anyhow::Result;

/// Generate shell export statements for the profile
pub fn generate_env(profile_name: Option<&str>, shell: &str) -> Result<()> {
    let profile = resolve_profile(profile_name)?;

    match shell {
        "bash" | "sh" | "zsh" => print_posix_exports(&profile),
        "fish" => print_fish_exports(&profile),
        "powershell" | "pwsh" => print_powershell_exports(&profile),
        _ => print_posix_exports(&profile),
    }

    Ok(())
}

fn print_posix_exports(profile: &Profile) {
    // triton section
    println!("# triton");
    println!("export TRITON_PROFILE=\"{}\"", profile.name);

    // docker section (placeholder for future docker host support)
    println!("# docker");

    // smartdc/SDC section for backwards compatibility
    println!("# smartdc");
    println!("export SDC_URL=\"{}\"", profile.url);
    println!("export SDC_ACCOUNT=\"{}\"", profile.account);

    if let Some(user) = &profile.user {
        println!("export SDC_USER=\"{}\"", user);
    } else {
        println!("unset SDC_USER");
    }

    println!("export SDC_KEY_ID=\"{}\"", profile.key_id);
    println!("unset SDC_TESTING");

    println!("# Run this command to configure your shell:");
    println!("#     eval \"$(triton env)\"");
}

fn print_fish_exports(profile: &Profile) {
    // triton section
    println!("# triton");
    println!("set -gx TRITON_PROFILE '{}'", profile.name);

    // docker section (placeholder for future docker host support)
    println!("# docker");

    // smartdc/SDC section for backwards compatibility
    println!("# smartdc");
    println!("set -gx SDC_URL '{}'", profile.url);
    println!("set -gx SDC_ACCOUNT '{}'", profile.account);

    if let Some(user) = &profile.user {
        println!("set -gx SDC_USER '{}'", user);
    } else {
        println!("set -e SDC_USER");
    }

    println!("set -gx SDC_KEY_ID '{}'", profile.key_id);
    println!("set -e SDC_TESTING");

    println!("# Run this command to configure your shell:");
    println!("#     triton env | source");
}

fn print_powershell_exports(profile: &Profile) {
    // triton section
    println!("# triton");
    println!("$env:TRITON_PROFILE = '{}'", profile.name);

    // docker section (placeholder for future docker host support)
    println!("# docker");

    // smartdc/SDC section for backwards compatibility
    println!("# smartdc");
    println!("$env:SDC_URL = '{}'", profile.url);
    println!("$env:SDC_ACCOUNT = '{}'", profile.account);

    if let Some(user) = &profile.user {
        println!("$env:SDC_USER = '{}'", user);
    } else {
        println!("Remove-Item Env:SDC_USER -ErrorAction SilentlyContinue");
    }

    println!("$env:SDC_KEY_ID = '{}'", profile.key_id);
    println!("Remove-Item Env:SDC_TESTING -ErrorAction SilentlyContinue");
}
