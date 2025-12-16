// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Profile management commands

use crate::config::{Config, Profile, paths, resolve_profile};
use crate::output::{json, table};
use anyhow::Result;
use clap::Subcommand;
use cloudapi_client::{AuthConfig, KeySource, TypedClient};
use dialoguer::{Confirm, Input};
use std::fs;
use std::path::PathBuf;
use triton_auth::{CertGenerator, CertPurpose, DEFAULT_CERT_LIFETIME_DAYS};

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

    /// Setup Docker TLS certificates for this profile
    ///
    /// Generate client TLS certificates for authenticating with the Triton
    /// Docker Engine. The certificates are stored in ~/.triton/docker/<profile>/
    DockerSetup {
        /// Profile name (defaults to current)
        name: Option<String>,
        /// Certificate lifetime in days (default: 3650 / 10 years)
        #[arg(short = 't', long, default_value_t = DEFAULT_CERT_LIFETIME_DAYS)]
        lifetime: u32,
        /// Skip confirmation prompts
        #[arg(short, long)]
        yes: bool,
    },

    /// Generate CMON client certificates for this profile
    ///
    /// Generate client TLS certificates for authenticating with the Triton
    /// Container Monitoring (CMON) service. The certificates are written to
    /// the current working directory.
    CmonCertgen {
        /// Profile name (defaults to current)
        name: Option<String>,
        /// Certificate lifetime in days (default: 3650 / 10 years)
        #[arg(short = 't', long, default_value_t = DEFAULT_CERT_LIFETIME_DAYS)]
        lifetime: u32,
        /// Skip confirmation prompts
        #[arg(short, long)]
        yes: bool,
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
            Self::DockerSetup {
                name,
                lifetime,
                yes,
            } => docker_setup(name, lifetime, yes).await,
            Self::CmonCertgen {
                name,
                lifetime,
                yes,
            } => cmon_certgen(name, lifetime, yes).await,
        }
    }
}

fn list_profiles(use_json: bool) -> Result<()> {
    use crate::config::env_profile;

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

/// Setup Docker TLS certificates for a profile
async fn docker_setup(name: Option<String>, lifetime: u32, yes: bool) -> Result<()> {
    // Resolve the profile
    let profile = resolve_profile(name.as_deref())?;
    let account = profile
        .act_as_account
        .as_deref()
        .unwrap_or(&profile.account);

    println!(
        "Setting up Docker for profile \"{}\" (account: {})",
        profile.name, account
    );

    // Check for Docker service
    let auth_config = AuthConfig::new(
        &profile.account,
        &profile.key_id,
        KeySource::auto(&profile.key_id),
    );
    let client = TypedClient::new(&profile.url, auth_config);

    println!("Checking for Docker service...");
    let services = client
        .inner()
        .list_services()
        .account(&profile.account)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list services: {}", e))?
        .into_inner();

    let docker_url = services
        .get("docker")
        .ok_or_else(|| anyhow::anyhow!("No Docker service available in this datacenter"))?;

    println!("Docker service found: {}", docker_url);

    // Warn about certificate generation
    println!();
    println!("WARNING: Docker uses authentication via client TLS certificates that do not");
    println!("support encrypted (passphrase protected) keys or SSH agents.");
    println!();
    println!("This action will create a fresh private key which is written unencrypted to");
    println!("disk in ~/.triton/docker/ for use by the Docker client. This key will be");
    println!("useable only for Docker.");
    println!();

    if !yes
        && !Confirm::new()
            .with_prompt("Continue?")
            .default(true)
            .interact()?
    {
        println!("Skipping Docker setup (you can run \"triton profile docker-setup\" later).");
        return Ok(());
    }

    // Generate certificates
    let generator = CertGenerator::new(&profile.key_id).map_err(|e| {
        anyhow::anyhow!(
            "Failed to setup certificate generator: {}. Make sure your SSH key is \
             loaded in the SSH agent and is not Ed25519 (use RSA or ECDSA).",
            e
        )
    })?;

    println!();
    println!(
        "Generating Docker certificates (key type: {})...",
        generator.key_type()
    );

    let cert = generator.generate(account, CertPurpose::Docker, lifetime)?;

    // Create directory for Docker certs
    let config_dir = paths::config_dir();
    let docker_dir = config_dir.join("docker").join(&profile.name);
    fs::create_dir_all(&docker_dir)?;

    // Write certificates
    let key_path = docker_dir.join("key.pem");
    let cert_path = docker_dir.join("cert.pem");
    fs::write(&key_path, &cert.key_pem)?;
    fs::write(&cert_path, &cert.cert_pem)?;

    // Download CA certificate from Docker host
    let ca_path = docker_dir.join("ca.pem");
    let ca_url = docker_url.replace("tcp:", "https:") + "/ca.pem";

    println!("Downloading CA certificate from {}...", ca_url);

    let ca_client = reqwest::ClientBuilder::new()
        .danger_accept_invalid_certs(profile.insecure)
        .build()?;

    let ca_response = ca_client
        .get(&ca_url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to download CA certificate: {}", e))?;

    let ca_pem = ca_response
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read CA certificate: {}", e))?;

    fs::write(&ca_path, &ca_pem)?;

    // Write setup.json for reference
    let setup_json = serde_json::json!({
        "profile": profile.name,
        "account": account,
        "time": chrono::Utc::now().to_rfc3339(),
        "env": {
            "DOCKER_CERT_PATH": docker_dir.to_string_lossy(),
            "DOCKER_HOST": docker_url,
            "DOCKER_TLS_VERIFY": if profile.insecure { serde_json::Value::Null } else { serde_json::json!("1") },
            "COMPOSE_HTTP_TIMEOUT": "300"
        }
    });
    let setup_path = docker_dir.join("setup.json");
    fs::write(&setup_path, serde_json::to_string_pretty(&setup_json)?)?;

    println!();
    println!(
        "Successfully setup profile \"{}\" to use Docker.",
        profile.name
    );
    println!();
    println!("To setup environment variables to use the Docker client, run:");
    println!("    eval \"$(triton env --docker {})\"", profile.name);
    println!("    docker info");
    println!();
    println!("Or you can place the commands in your shell profile, e.g.:");
    println!("    triton env --docker {} >> ~/.profile", profile.name);

    Ok(())
}

/// Generate CMON client certificates for a profile
async fn cmon_certgen(name: Option<String>, lifetime: u32, yes: bool) -> Result<()> {
    // Resolve the profile
    let profile = resolve_profile(name.as_deref())?;
    let account = profile
        .act_as_account
        .as_deref()
        .unwrap_or(&profile.account);

    println!(
        "Generating CMON certificates for profile \"{}\" (account: {})",
        profile.name, account
    );

    // Check for CMON service
    let auth_config = AuthConfig::new(
        &profile.account,
        &profile.key_id,
        KeySource::auto(&profile.key_id),
    );
    let client = TypedClient::new(&profile.url, auth_config);

    println!("Checking for CMON service...");
    let services = client
        .inner()
        .list_services()
        .account(&profile.account)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list services: {}", e))?
        .into_inner();

    let cmon_url = services
        .get("cmon")
        .ok_or_else(|| anyhow::anyhow!("No CMON service available in this datacenter"))?;

    println!("CMON service found: {}", cmon_url);

    // Warn about certificate generation
    println!();
    println!("Note: CMON uses authentication via client TLS certificates.");
    println!();
    println!("This action will create a fresh private key which is written unencrypted to");
    println!("disk in the current working directory. Copy these files to your CMON client");
    println!("(whether Prometheus, or something else).");
    println!();
    println!("This key will be usable only for CMON. If your SSH key is removed from your");
    println!("account, this CMON key will no longer work.");
    println!();

    if !yes {
        println!("If you do not specifically want to use CMON, or want to set this up later,");
        println!("you can answer \"no\" here.");
        println!();
    }

    if !yes
        && !Confirm::new()
            .with_prompt("Continue?")
            .default(true)
            .interact()?
    {
        println!(
            "Skipping CMON certificate generation (you can run \"triton profile cmon-certgen\" later)."
        );
        return Ok(());
    }

    // Generate certificates
    let generator = CertGenerator::new(&profile.key_id).map_err(|e| {
        anyhow::anyhow!(
            "Failed to setup certificate generator: {}. Make sure your SSH key is \
             loaded in the SSH agent and is not Ed25519 (use RSA or ECDSA).",
            e
        )
    })?;

    println!();
    println!(
        "Generating CMON certificates (key type: {})...",
        generator.key_type()
    );

    let cert = generator.generate(account, CertPurpose::Cmon, lifetime)?;

    // Write certificates to current directory
    let fn_stub = format!("cmon-{}", account);
    let key_path = PathBuf::from(format!("{}-key.pem", fn_stub));
    let cert_path = PathBuf::from(format!("{}-cert.pem", fn_stub));
    fs::write(&key_path, &cert.key_pem)?;
    fs::write(&cert_path, &cert.cert_pem)?;

    // Generate example Prometheus configuration
    let cmon_host = cmon_url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let prometheus_yml = format!(
        r#"global:
    scrape_interval: 15s
    scrape_timeout: 10s
    evaluation_interval: 15s
scrape_configs:
    - job_name: triton-{account}
      scheme: https
      tls_config:
          cert_file: {fn_stub}-cert.pem
          key_file: {fn_stub}-key.pem
      relabel_configs:
          - source_labels: [__meta_triton_machine_alias]
            target_label: alias
          - source_labels: [__meta_triton_machine_id]
            target_label: instance
      triton_sd_configs:
          - account: {account}
            dns_suffix: {cmon_host}
            endpoint: {cmon_host}
            version: 1
            tls_config:
                cert_file: {fn_stub}-cert.pem
                key_file: {fn_stub}-key.pem
"#,
        account = account,
        fn_stub = fn_stub,
        cmon_host = cmon_host,
    );

    let prometheus_path = PathBuf::from(format!("{}-prometheus.yml", fn_stub));
    fs::write(&prometheus_path, &prometheus_yml)?;

    println!();
    println!("CMON authentication certificate and key have been placed in files");
    println!(
        "\"{}\" and \"{}\".",
        cert_path.display(),
        key_path.display()
    );
    println!();
    println!("An example Prometheus configuration file has also been written into");
    println!("\"{}\".", prometheus_path.display());
    println!();
    println!("It can be used as-is for testing by running");
    println!(
        "  \"prometheus --config.file={}\"",
        prometheus_path.display()
    );

    Ok(())
}
