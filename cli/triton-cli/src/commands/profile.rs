// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Profile management commands

use crate::config::{Config, Profile};
use crate::output::{json, table};
use anyhow::Result;
use clap::Subcommand;
use dialoguer::{Confirm, Input};

#[derive(Subcommand, Clone)]
pub enum ProfileCommand {
    /// List all profiles
    #[command(alias = "ls")]
    List {
        /// Output as JSON
        #[arg(short, long)]
        json: bool,
    },

    /// Get current profile details
    Get {
        /// Profile name (defaults to current)
        name: Option<String>,
        /// Output as JSON
        #[arg(short, long)]
        json: bool,
    },

    /// Create a new profile
    Create {
        /// Profile name
        name: Option<String>,
        /// CloudAPI URL
        #[arg(long)]
        url: Option<String>,
        /// Account name
        #[arg(long, short)]
        account: Option<String>,
        /// SSH key fingerprint
        #[arg(long, short)]
        key_id: Option<String>,
        /// Skip TLS verification
        #[arg(long)]
        insecure: bool,
    },

    /// Edit an existing profile
    Edit {
        /// Profile name
        name: String,
    },

    /// Delete a profile
    #[command(alias = "rm")]
    Delete {
        /// Profile name(s)
        names: Vec<String>,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Set the current profile
    SetCurrent {
        /// Profile name (use '-' for previous)
        name: String,
    },
}

impl ProfileCommand {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::List { json: use_json } => list_profiles(use_json),
            Self::Get {
                name,
                json: use_json,
            } => get_profile(name, use_json),
            Self::Create {
                name,
                url,
                account,
                key_id,
                insecure,
            } => create_profile(name, url, account, key_id, insecure),
            Self::Edit { name } => edit_profile(&name),
            Self::Delete { names, force } => delete_profiles(&names, force),
            Self::SetCurrent { name } => set_current_profile(&name),
        }
    }
}

fn list_profiles(use_json: bool) -> Result<()> {
    use crate::config::{env_profile, resolve_profile};

    let saved_profiles = Profile::list_all()?;

    // Try to get the current profile (might be "env" from environment variables)
    let current_profile = resolve_profile(None).ok();
    let current_name = current_profile.as_ref().map(|p| p.name.as_str());

    // Build list of profiles to display, including "env" if it's the current one
    let mut profiles_to_show: Vec<Profile> = Vec::new();

    // Add "env" profile if environment variables are set
    if let Ok(env_prof) = env_profile() {
        profiles_to_show.push(env_prof);
    }

    // Add saved profiles
    for name in &saved_profiles {
        if let Ok(profile) = Profile::load(name) {
            profiles_to_show.push(profile);
        }
    }

    if use_json {
        json::print_json(&profiles_to_show)?;
    } else {
        let mut tbl = table::create_table(&["NAME", "CURR", "ACCOUNT", "USER", "URL"]);
        for profile in &profiles_to_show {
            let marker = if Some(profile.name.as_str()) == current_name {
                "*"
            } else {
                ""
            };
            let user = profile.user.as_deref().unwrap_or("-");
            tbl.add_row(vec![
                profile.name.as_str(),
                marker,
                profile.account.as_str(),
                user,
                profile.url.as_str(),
            ]);
        }
        table::print_table(tbl);
    }
    Ok(())
}

fn get_profile(name: Option<String>, use_json: bool) -> Result<()> {
    let profile = match name {
        Some(n) => Profile::load(&n)?,
        None => {
            let config = Config::load()?;
            let current = config
                .current_profile()
                .ok_or_else(|| anyhow::anyhow!("No current profile set"))?;
            Profile::load(current)?
        }
    };

    if use_json {
        json::print_json(&profile)?;
    } else {
        println!("Name:     {}", profile.name);
        println!("URL:      {}", profile.url);
        println!("Account:  {}", profile.account);
        println!("Key ID:   {}", profile.key_id);
        println!("Insecure: {}", profile.insecure);
        if let Some(user) = &profile.user {
            println!("User:     {}", user);
        }
        if let Some(roles) = &profile.roles {
            println!("Roles:    {}", roles.join(", "));
        }
    }
    Ok(())
}

fn create_profile(
    name: Option<String>,
    url: Option<String>,
    account: Option<String>,
    key_id: Option<String>,
    insecure: bool,
) -> Result<()> {
    // Interactive prompts for missing values
    let name = match name {
        Some(n) => n,
        None => Input::new().with_prompt("Profile name").interact_text()?,
    };

    // Check if profile already exists
    if Profile::list_all()?.contains(&name) {
        return Err(anyhow::anyhow!("Profile '{}' already exists", name));
    }

    let url = match url {
        Some(u) => u,
        None => Input::new()
            .with_prompt("CloudAPI URL")
            .default("https://cloudapi.tritondatacenter.com".to_string())
            .interact_text()?,
    };

    let account = match account {
        Some(a) => a,
        None => Input::new().with_prompt("Account name").interact_text()?,
    };

    let key_id = match key_id {
        Some(k) => k,
        None => Input::new()
            .with_prompt("SSH key fingerprint (aa:bb:cc:... or SHA256:...)")
            .interact_text()?,
    };

    let profile = Profile {
        name: name.clone(),
        url,
        account,
        key_id,
        insecure,
        user: None,
        roles: None,
        act_as_account: None,
    };

    profile.save()?;
    println!("Created profile '{}'", name);

    // Ask if this should be the current profile
    if Confirm::new()
        .with_prompt("Set as current profile?")
        .default(true)
        .interact()?
    {
        let mut config = Config::load()?;
        config.set_current_profile(&name);
        config.save()?;
        println!("Set '{}' as current profile", name);
    }

    Ok(())
}

fn edit_profile(name: &str) -> Result<()> {
    let mut profile = Profile::load(name)?;

    profile.url = Input::new()
        .with_prompt("CloudAPI URL")
        .default(profile.url)
        .interact_text()?;

    profile.account = Input::new()
        .with_prompt("Account name")
        .default(profile.account)
        .interact_text()?;

    profile.key_id = Input::new()
        .with_prompt("SSH key fingerprint")
        .default(profile.key_id)
        .interact_text()?;

    profile.insecure = Confirm::new()
        .with_prompt("Skip TLS verification?")
        .default(profile.insecure)
        .interact()?;

    profile.save()?;
    println!("Updated profile '{}'", name);
    Ok(())
}

fn delete_profiles(names: &[String], force: bool) -> Result<()> {
    for name in names {
        if !force
            && !Confirm::new()
                .with_prompt(format!("Delete profile '{}'?", name))
                .default(false)
                .interact()?
        {
            continue;
        }
        Profile::delete(name)?;
        println!("Deleted profile '{}'", name);
    }
    Ok(())
}

fn set_current_profile(name: &str) -> Result<()> {
    let mut config = Config::load()?;

    let name = if name == "-" {
        config
            .old_profile
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No previous profile"))?
    } else {
        // Verify profile exists
        Profile::load(name)?;
        name.to_string()
    };

    config.set_current_profile(&name);
    config.save()?;
    println!("Current profile: {}", name);
    Ok(())
}
