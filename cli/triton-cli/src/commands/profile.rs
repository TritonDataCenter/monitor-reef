// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Profile management commands

use crate::config::{Config, Profile, SshKeyProfile, paths, resolve_profile};
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};
use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::{AuthConfig, KeySource, TypedClient};
use dialoguer::{Confirm, Input};
use std::fs;
use std::path::PathBuf;
use triton_auth::{CertGenerator, CertPurpose, DEFAULT_CERT_LIFETIME_DAYS};

#[derive(Args, Clone)]
pub struct ProfileListArgs {
    /// Output as JSON
    #[arg(short, long)]
    pub json: bool,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Subcommand, Clone)]
pub enum ProfileCommand {
    /// List all profiles
    #[command(visible_alias = "ls")]
    List(ProfileListArgs),

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
        /// Create profile from JSON file (use '-' for stdin)
        #[arg(short = 'f', long = "file", conflicts_with_all = ["name", "url", "account", "key_id", "copy"])]
        file: Option<PathBuf>,
        /// Copy values from an existing profile
        #[arg(long, conflicts_with = "file")]
        copy: Option<String>,
        /// Skip Docker setup (Docker setup is not yet implemented; this flag is accepted for compatibility)
        #[arg(long)]
        no_docker: bool,
        /// Answer yes to any confirmations (non-interactive mode)
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Edit an existing profile
    Edit {
        /// Profile name
        name: String,
    },

    /// Delete a profile
    #[command(visible_alias = "rm")]
    Delete {
        /// Profile name(s)
        names: Vec<String>,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Set the current profile
    #[command(alias = "set")]
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
            Self::List(args) => list_profiles(args).await,
            Self::Get {
                name,
                json: use_json,
            } => get_profile(name, use_json).await,
            Self::Create {
                name,
                url,
                account,
                key_id,
                insecure,
                file,
                copy,
                no_docker: _no_docker,
                yes,
            } => create_profile(name, url, account, key_id, insecure, file, copy, yes).await,
            Self::Edit { name } => edit_profile(&name).await,
            Self::Delete { names, force } => delete_profiles(&names, force).await,
            Self::SetCurrent { name } => set_current_profile(&name).await,
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

async fn list_profiles(args: ProfileListArgs) -> Result<()> {
    use crate::config::env_profile;

    let saved_profiles = Profile::list_all().await?;

    // Try to get the current profile (might be "env" from environment variables)
    let current_profile = resolve_profile(None).await.ok();
    let current_name = current_profile.as_ref().map(|p| p.name().to_string());

    // Build list of profiles to display, including "env" if it's the current one
    let mut profiles_to_show: Vec<Profile> = Vec::new();

    // Add "env" profile if environment variables are set
    if let Ok(env_prof) = env_profile() {
        profiles_to_show.push(env_prof);
    }

    // Add saved profiles
    for name in &saved_profiles {
        match Profile::load(name).await {
            Ok(profile) => profiles_to_show.push(profile),
            // arch-lint: allow(no-error-swallowing) reason="One corrupt profile should not prevent listing the rest"
            Err(e) => {
                eprintln!("warning: skipping profile '{name}': {e}");
            }
        }
    }

    if args.json {
        for profile in &profiles_to_show {
            let is_curr = current_name.as_deref() == Some(profile.name());
            let mut value = serde_json::to_value(profile)?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("curr".to_string(), serde_json::Value::Bool(is_curr));
            }
            json::print_json(&value)?;
        }
    } else {
        // Table keeps the legacy SSH-flavored column set (KEYID / USER) for
        // compatibility with operators' existing scripts; tritonapi
        // profiles fill those cells with "-".
        let mut tbl = TableBuilder::new(&["NAME", "CURR", "ACCOUNT", "USER", "URL"])
            .with_long_headers(&["KEYID", "INSECURE"]);
        for profile in &profiles_to_show {
            let marker = if current_name.as_deref() == Some(profile.name()) {
                "*"
            } else {
                ""
            };
            let (user, key_id) = match profile.as_ssh_key() {
                Some(ssh) => (
                    ssh.user.as_deref().unwrap_or("-").to_string(),
                    ssh.key_id.clone(),
                ),
                None => ("-".to_string(), "-".to_string()),
            };
            tbl.add_row(vec![
                profile.name().to_string(),
                marker.to_string(),
                profile.account().to_string(),
                user,
                profile.url().to_string(),
                key_id,
                profile.insecure().to_string(),
            ]);
        }
        tbl.print(&args.table)?;
    }
    Ok(())
}

async fn get_profile(name: Option<String>, use_json: bool) -> Result<()> {
    let profile = resolve_profile(name.as_deref()).await?;

    // Determine if this is the current profile
    let is_curr = if name.is_none() {
        true // No name specified = resolved to current profile
    } else {
        resolve_profile(None)
            .await
            .ok()
            .is_some_and(|current| current.name() == profile.name())
    };

    if use_json {
        // Build JSON with curr field, matching node-triton format
        let mut value = serde_json::to_value(&profile)?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("curr".to_string(), serde_json::Value::Bool(is_curr));
        }
        json::print_json(&value)?;
    } else {
        // Match node-triton text format: lowercase labels, no padding
        println!("name: {}", profile.name());
        println!("account: {}", profile.account());
        println!("curr: {}", is_curr);
        match &profile {
            Profile::SshKey(ssh) => {
                println!("keyId: {}", ssh.key_id);
                println!("url: {}", ssh.url);
                if ssh.insecure {
                    println!("insecure: {}", ssh.insecure);
                }
                if let Some(user) = &ssh.user {
                    println!("user: {}", user);
                }
                if let Some(roles) = &ssh.roles {
                    println!("roles: {}", roles.join(", "));
                }
            }
            Profile::TritonApi(api) => {
                println!("auth: tritonapi");
                println!("url: {}", api.url);
                if api.insecure {
                    println!("insecure: {}", api.insecure);
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn create_profile(
    name: Option<String>,
    url: Option<String>,
    account: Option<String>,
    key_id: Option<String>,
    insecure: bool,
    file: Option<PathBuf>,
    copy: Option<String>,
    yes: bool,
) -> Result<()> {
    // If file is provided, create from file/stdin
    if let Some(file_path) = file {
        use std::io::Read;

        let content = if file_path.as_os_str() == "-" {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        } else {
            tokio::fs::read_to_string(&file_path).await.map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read profile file '{}': {}",
                    file_path.display(),
                    e
                )
            })?
        };

        let mut profile: Profile = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse profile JSON: {}", e))?;

        // Derive profile name from the filename if not present in JSON
        // (node-triton profiles don't include a "name" field)
        if profile.name().is_empty() {
            if file_path.as_os_str() == "-" {
                return Err(anyhow::anyhow!(
                    "Profile JSON from stdin must include a \"name\" field"
                ));
            }
            let stem = file_path
                .file_stem()
                .ok_or_else(|| anyhow::anyhow!("Cannot derive profile name from file path"))?
                .to_string_lossy()
                .to_string();
            profile.set_name(stem);
        }

        // Check if profile already exists
        if Profile::list_all()
            .await?
            .iter()
            .any(|n| n == profile.name())
        {
            return Err(anyhow::anyhow!(
                "Profile '{}' already exists",
                profile.name()
            ));
        }

        let profile_name = profile.name().to_string();
        profile.save().await?;
        println!("Created profile '{}' from file", profile_name);

        // Ask if this should be the current profile (skip if --yes)
        if yes
            || Confirm::new()
                .with_prompt("Set as current profile?")
                .default(true)
                .interact()?
        {
            let mut config = Config::load().await?;
            config.set_current_profile(&profile_name);
            config.save().await?;
            println!("Set '{}' as current profile", profile_name);
        }

        return Ok(());
    }

    // Load defaults from source profile if --copy is specified.
    //
    // `profile create` (without --file) is SSH-flavored — it prompts for an
    // SSH key fingerprint. A tritonapi copy source has no key fingerprint
    // so we reject it with a clear message rather than silently dropping
    // data.
    let copy_profile: Option<SshKeyProfile> = if let Some(ref copy_name) = copy {
        let loaded = Profile::load(copy_name).await.map_err(|_| {
            anyhow::anyhow!("no such profile from which to copy: \"{}\"", copy_name)
        })?;
        match loaded {
            Profile::SshKey(ssh) => Some(ssh),
            Profile::TritonApi(_) => {
                return Err(anyhow::anyhow!(
                    "cannot --copy from tritonapi profile '{}' into an SSH profile; \
                     use a different source or edit the target profile file directly",
                    copy_name
                ));
            }
        }
    } else {
        None
    };

    // Interactive prompts for missing values (with defaults from copy profile)
    let name = match name {
        Some(n) => n,
        None if yes => {
            return Err(anyhow::anyhow!(
                "Profile name is required in non-interactive mode"
            ));
        }
        None => Input::new().with_prompt("Profile name").interact_text()?,
    };

    // Check if profile already exists
    if Profile::list_all().await?.contains(&name) {
        return Err(anyhow::anyhow!("Profile '{}' already exists", name));
    }

    let default_url = copy_profile
        .as_ref()
        .map(|p| p.url.clone())
        .unwrap_or_else(|| "https://cloudapi.tritondatacenter.com".to_string());
    let url = match url {
        Some(u) => u,
        None if yes => default_url,
        None => Input::new()
            .with_prompt("CloudAPI URL")
            .default(default_url)
            .interact_text()?,
    };

    let default_account = copy_profile.as_ref().map(|p| p.account.clone());
    let account = match account {
        Some(a) => a,
        None if yes => default_account
            .ok_or_else(|| anyhow::anyhow!("Account name is required in non-interactive mode"))?,
        None => {
            let mut input = Input::new().with_prompt("Account name");
            if let Some(default) = default_account {
                input = input.default(default);
            }
            input.interact_text()?
        }
    };

    let default_key_id = copy_profile.as_ref().map(|p| p.key_id.clone());
    let key_id = match key_id {
        Some(k) => k,
        None if yes => default_key_id.ok_or_else(|| {
            anyhow::anyhow!("SSH key fingerprint is required in non-interactive mode")
        })?,
        None => {
            let mut input =
                Input::new().with_prompt("SSH key fingerprint (aa:bb:cc:... or SHA256:...)");
            if let Some(default) = default_key_id {
                input = input.default(default);
            }
            input.interact_text()?
        }
    };

    // Copy additional fields from source profile if available
    let (user, roles, act_as_account) = if let Some(ref source) = copy_profile {
        (
            source.user.clone(),
            source.roles.clone(),
            source.act_as_account.clone(),
        )
    } else {
        (None, None, None)
    };

    let profile = Profile::SshKey(SshKeyProfile {
        name: name.clone(),
        url,
        account,
        key_id,
        insecure,
        user,
        roles,
        act_as_account,
    });

    profile.save().await?;
    println!("Created profile '{}'", name);

    // Check if this is the only profile - if so, set it as current automatically
    let existing_profiles = Profile::list_all().await?;
    if existing_profiles.len() == 1 && existing_profiles.contains(&name) {
        let mut config = Config::load().await?;
        config.set_current_profile(&name);
        config.save().await?;
        println!(
            "Set '{}' as current profile (because it is your only profile)",
            name
        );
    } else if yes
        || Confirm::new()
            .with_prompt("Set as current profile?")
            .default(true)
            .interact()?
    {
        let mut config = Config::load().await?;
        config.set_current_profile(&name);
        config.save().await?;
        println!("Set '{}' as current profile", name);
    }

    Ok(())
}

async fn edit_profile(name: &str) -> Result<()> {
    let mut profile = Profile::load(name).await?;

    // Interactive edit is SSH-flavored (prompts for an SSH key fingerprint
    // and the cloudapi URL). We don't expose an interactive editor for
    // tritonapi profiles — operators who need to change the gateway URL
    // can edit the file directly, and credential rotation happens via
    // `triton login`.
    let ssh = profile.as_ssh_key_mut().ok_or_else(|| {
        anyhow::anyhow!(
            "'triton profile edit {}' is only supported for SSH-key profiles; \
             the tritonapi profile '{}' should be modified by editing its JSON \
             file directly (~/.triton/profiles.d/{}.json)",
            name,
            name,
            name
        )
    })?;

    ssh.url = Input::new()
        .with_prompt("CloudAPI URL")
        .default(ssh.url.clone())
        .interact_text()?;

    ssh.account = Input::new()
        .with_prompt("Account name")
        .default(ssh.account.clone())
        .interact_text()?;

    ssh.key_id = Input::new()
        .with_prompt("SSH key fingerprint")
        .default(ssh.key_id.clone())
        .interact_text()?;

    ssh.insecure = Confirm::new()
        .with_prompt("Skip TLS verification?")
        .default(ssh.insecure)
        .interact()?;

    profile.save().await?;
    println!("Updated profile '{}'", name);
    Ok(())
}

async fn delete_profiles(names: &[String], force: bool) -> Result<()> {
    for name in names {
        if !force
            && !Confirm::new()
                .with_prompt(format!("Delete profile '{}'?", name))
                .default(false)
                .interact()?
        {
            continue;
        }
        Profile::delete(name).await?;
        println!("Deleted profile '{}'", name);
    }
    Ok(())
}

async fn set_current_profile(name: &str) -> Result<()> {
    let mut config = Config::load().await?;

    let name = if name == "-" {
        config
            .old_profile
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No previous profile"))?
    } else {
        // Verify profile exists (resolve_profile handles the "env" profile)
        resolve_profile(Some(name)).await?;
        name.to_string()
    };

    if config.current_profile() == Some(&name) {
        println!("\"{}\" is already the current profile", name);
    } else {
        config.set_current_profile(&name);
        config.save().await?;
        println!("Set \"{}\" as current profile", name);
    }
    Ok(())
}

/// Setup Docker TLS certificates for a profile
async fn docker_setup(name: Option<String>, lifetime: u32, yes: bool) -> Result<()> {
    // Resolve the profile. Docker auth uses client TLS certs derived from
    // the profile's SSH key, so this command requires SSH-kind profiles.
    let profile = resolve_profile(name.as_deref()).await?;
    let profile_name = profile.name().to_string();
    let ssh = profile.require_ssh_key()?;
    let account = ssh.act_as_account.as_deref().unwrap_or(&ssh.account);

    println!(
        "Setting up Docker for profile \"{}\" (account: {})",
        profile_name, account
    );

    // Check for Docker service
    let auth_config = AuthConfig::new(&ssh.account, KeySource::auto(&ssh.key_id));
    let client = TypedClient::new(&ssh.url, auth_config);

    println!("Checking for Docker service...");
    let services = client
        .inner()
        .list_services()
        .account(&ssh.account)
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
    let generator = CertGenerator::new(&ssh.key_id).map_err(|e| {
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
    let docker_dir = config_dir.join("docker").join(&profile_name);
    fs::create_dir_all(&docker_dir)?;

    // Write certificates
    let key_path = docker_dir.join("key.pem");
    let cert_path = docker_dir.join("cert.pem");
    fs::write(&key_path, &cert.key_pem)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }
    fs::write(&cert_path, &cert.cert_pem)?;

    // Download CA certificate from Docker host
    let ca_path = docker_dir.join("ca.pem");
    let ca_url = docker_url.replace("tcp:", "https:") + "/ca.pem";

    println!("Downloading CA certificate from {}...", ca_url);

    let ca_client = reqwest::ClientBuilder::new()
        .danger_accept_invalid_certs(ssh.insecure)
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
        "profile": profile_name,
        "account": account,
        "time": chrono::Utc::now().to_rfc3339(),
        "env": {
            "DOCKER_CERT_PATH": docker_dir.to_string_lossy(),
            "DOCKER_HOST": docker_url,
            "DOCKER_TLS_VERIFY": if ssh.insecure { serde_json::Value::Null } else { serde_json::json!("1") },
            "COMPOSE_HTTP_TIMEOUT": "300"
        }
    });
    let setup_path = docker_dir.join("setup.json");
    fs::write(&setup_path, serde_json::to_string_pretty(&setup_json)?)?;

    println!();
    println!(
        "Successfully setup profile \"{}\" to use Docker.",
        profile_name
    );
    println!();
    println!("To setup environment variables to use the Docker client, run:");
    println!("    eval \"$(triton env --docker {})\"", profile_name);
    println!("    docker info");
    println!();
    println!("Or you can place the commands in your shell profile, e.g.:");
    println!("    triton env --docker {} >> ~/.profile", profile_name);

    Ok(())
}

/// Generate CMON client certificates for a profile
async fn cmon_certgen(name: Option<String>, lifetime: u32, yes: bool) -> Result<()> {
    // Resolve the profile. CMON, like Docker, authenticates with client
    // TLS certs derived from the SSH key, so this requires SSH-kind.
    let profile = resolve_profile(name.as_deref()).await?;
    let profile_name = profile.name().to_string();
    let ssh = profile.require_ssh_key()?;
    let account = ssh.act_as_account.as_deref().unwrap_or(&ssh.account);

    println!(
        "Generating CMON certificates for profile \"{}\" (account: {})",
        profile_name, account
    );

    // Check for CMON service
    let auth_config = AuthConfig::new(&ssh.account, KeySource::auto(&ssh.key_id));
    let client = TypedClient::new(&ssh.url, auth_config);

    println!("Checking for CMON service...");
    let services = client
        .inner()
        .list_services()
        .account(&ssh.account)
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
    let generator = CertGenerator::new(&ssh.key_id).map_err(|e| {
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }
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
