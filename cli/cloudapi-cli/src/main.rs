// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! CloudAPI CLI - Command-line interface for Triton CloudAPI
//!
//! This CLI provides access to all CloudAPI endpoints for managing virtual machines,
//! images, networks, volumes, and other resources in Triton.
//!
//! # Environment Variables
//!
//! The CLI reads configuration from environment variables compatible with the
//! standard Triton CLI tools:
//!
//! - `TRITON_URL` or `CLOUDAPI_URL` - CloudAPI base URL
//! - `TRITON_ACCOUNT` or `CLOUDAPI_ACCOUNT` - Account name
//! - `TRITON_KEY_ID` - SSH key fingerprint (for future authentication support)
//!
//! The `CLOUDAPI_*` variants take precedence over `TRITON_*` variants.

use anyhow::Result;
use clap::{Parser, Subcommand};
use cloudapi_api::{DOCS_URL, FAVICON_URL};
use cloudapi_client::{AuthConfig, KeySource, TypedClient};

/// Get environment variable with fallback
///
/// Tries the primary variable first, then falls back to the secondary.
fn env_with_fallback(primary: &str, fallback: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .or_else(|| std::env::var(fallback).ok())
}

#[derive(Parser)]
#[command(name = "cloudapi", version, about = "CLI for Triton CloudAPI")]
struct Cli {
    /// CloudAPI base URL
    ///
    /// Can also be set via CLOUDAPI_URL or TRITON_URL environment variables.
    /// CLOUDAPI_URL takes precedence over TRITON_URL.
    #[arg(long)]
    base_url: Option<String>,

    /// Account name
    ///
    /// Can also be set via CLOUDAPI_ACCOUNT or TRITON_ACCOUNT environment variables.
    /// CLOUDAPI_ACCOUNT takes precedence over TRITON_ACCOUNT.
    #[arg(long)]
    account: Option<String>,

    /// SSH key fingerprint for authentication
    ///
    /// Can also be set via CLOUDAPI_KEY_ID or TRITON_KEY_ID environment variables.
    /// CLOUDAPI_KEY_ID takes precedence over TRITON_KEY_ID.
    /// Supports both MD5 (aa:bb:cc:...) and SHA256 (SHA256:base64...) formats.
    #[arg(long)]
    key_id: Option<String>,

    /// Path to SSH private key file (optional)
    ///
    /// If not specified, keys are loaded from SSH agent or ~/.ssh/ directory.
    #[arg(long)]
    key_path: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Get account details
    GetAccount {
        #[arg(long)]
        raw: bool,
    },
    /// List machines
    ListMachines {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        raw: bool,
    },
    /// Get machine details
    GetMachine {
        machine: String,
        #[arg(long)]
        raw: bool,
    },
    /// Start a machine
    StartMachine { machine: String },
    /// Stop a machine
    StopMachine { machine: String },
    /// List images
    ListImages {
        #[arg(long)]
        raw: bool,
    },
    /// Get image
    GetImage {
        image: String,
        #[arg(long)]
        raw: bool,
    },
    /// List packages
    ListPackages {
        #[arg(long)]
        raw: bool,
    },
    /// List networks
    ListNetworks {
        #[arg(long)]
        raw: bool,
    },
    /// List volumes
    ListVolumes {
        #[arg(long)]
        raw: bool,
    },
    /// List firewall rules
    ListFirewallRules {
        #[arg(long)]
        raw: bool,
    },
    /// List services
    ListServices {
        #[arg(long)]
        raw: bool,
    },
    /// List datacenters
    ListDatacenters {
        #[arg(long)]
        raw: bool,
    },
    /// Test documentation redirect endpoints
    ///
    /// Verifies that the documentation redirect endpoints return HTTP 302
    /// with correct Location headers. Does not require authentication.
    ///
    /// Note: These endpoints (/, /docs, /favicon.ico) cannot be included in
    /// the Dropshot API trait due to routing conflicts with /{account}. They
    /// must be handled at the reverse proxy or HTTP server level. This command
    /// tests that they are properly configured.
    TestDocs,
}

const DEFAULT_BASE_URL: &str = "https://cloudapi.tritondatacenter.com";

/// Resolve the account from CLI arg or environment variables
fn resolve_account(cli_account: Option<String>) -> Result<String> {
    cli_account
        .or_else(|| env_with_fallback("CLOUDAPI_ACCOUNT", "TRITON_ACCOUNT"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Account required. Set via --account flag, CLOUDAPI_ACCOUNT, or TRITON_ACCOUNT environment variable"
            )
        })
}

/// Resolve the base URL from CLI arg or environment variables
fn resolve_base_url(cli_base_url: Option<String>) -> String {
    cli_base_url
        .or_else(|| env_with_fallback("CLOUDAPI_URL", "TRITON_URL"))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

/// Resolve the key ID from CLI arg or environment variables
fn resolve_key_id(cli_key_id: Option<String>) -> Result<String> {
    cli_key_id
        .or_else(|| env_with_fallback("CLOUDAPI_KEY_ID", "TRITON_KEY_ID"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "SSH key fingerprint required for authentication. Set via --key-id flag, CLOUDAPI_KEY_ID, or TRITON_KEY_ID environment variable"
            )
        })
}

/// Create AuthConfig from CLI arguments and environment variables
fn create_auth_config(account: &str, key_id: &str, key_path: Option<String>) -> AuthConfig {
    let key_source = match key_path {
        Some(path) => KeySource::file(path),
        None => KeySource::auto(key_id),
    };
    AuthConfig::new(account, key_id, key_source)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let base_url = resolve_base_url(cli.base_url);

    // Handle TestDocs separately since it doesn't require authentication
    if matches!(cli.command, Commands::TestDocs) {
        return test_docs(&base_url).await;
    }

    // Resolve account and key_id for authentication
    let account = resolve_account(cli.account)?;
    let key_id = resolve_key_id(cli.key_id)?;
    let auth_config = create_auth_config(&account, &key_id, cli.key_path);
    let client = TypedClient::new(&base_url, auth_config);

    match cli.command {
        Commands::GetAccount { raw } => {
            let resp = client
                .inner()
                .get_account()
                .account(&account)
                .send()
                .await?;
            let acc = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&acc)?);
            } else {
                println!("Account: {}", serde_json::to_string_pretty(&acc)?);
            }
        }
        Commands::ListMachines { name, raw } => {
            let mut req = client.inner().list_machines().account(&account);
            if let Some(n) = name {
                req = req.name(n);
            }
            let resp = req.send().await?;
            let machines = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&machines)?);
            } else {
                for m in &machines {
                    println!("{}: {} ({})", m.id, m.name, m.state);
                }
            }
        }
        Commands::GetMachine { machine, raw } => {
            let resp = client
                .inner()
                .get_machine()
                .account(&account)
                .machine(machine)
                .send()
                .await?;
            let m = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&m)?);
            } else {
                println!(
                    "ID: {}\nName: {}\nState: {}\nImage: {}",
                    m.id, m.name, m.state, m.image
                );
            }
        }
        Commands::StartMachine { machine } => {
            client.start_machine(&account, &machine, None).await?;
            println!("Machine started");
        }
        Commands::StopMachine { machine } => {
            client.stop_machine(&account, &machine, None).await?;
            println!("Machine stopped");
        }
        Commands::ListImages { raw } => {
            let resp = client
                .inner()
                .list_images()
                .account(&account)
                .send()
                .await?;
            let images = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&images)?);
            } else {
                for img in &images {
                    let state_str = img
                        .state
                        .as_ref()
                        .map(|s| format!("{:?}", s))
                        .unwrap_or_else(|| "unknown".to_string());
                    println!("{}: {} {} ({})", img.id, img.name, img.version, state_str);
                }
            }
        }
        Commands::GetImage { image, raw } => {
            let resp = client
                .inner()
                .get_image()
                .account(&account)
                .dataset(image)
                .send()
                .await?;
            let img = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&img)?);
            } else {
                let state_str = img
                    .state
                    .as_ref()
                    .map(|s| format!("{:?}", s))
                    .unwrap_or_else(|| "unknown".to_string());
                println!(
                    "ID: {}\nName: {}\nVersion: {}\nState: {}",
                    img.id, img.name, img.version, state_str
                );
            }
        }
        Commands::ListPackages { raw } => {
            let resp = client
                .inner()
                .list_packages()
                .account(&account)
                .send()
                .await?;
            let pkgs = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&pkgs)?);
            } else {
                for pkg in &pkgs {
                    println!("{}: {} (memory: {})", pkg.id, pkg.name, pkg.memory);
                }
            }
        }
        Commands::ListNetworks { raw } => {
            let resp = client
                .inner()
                .list_networks()
                .account(&account)
                .send()
                .await?;
            let nets = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&nets)?);
            } else {
                for net in &nets {
                    println!("{}: {}", net.id, net.name);
                }
            }
        }
        Commands::ListVolumes { raw } => {
            let resp = client
                .inner()
                .list_volumes()
                .account(&account)
                .send()
                .await?;
            let vols = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&vols)?);
            } else {
                for vol in &vols {
                    println!("{}: {} ({})", vol.id, vol.name, vol.state);
                }
            }
        }
        Commands::ListFirewallRules { raw } => {
            let resp = client
                .inner()
                .list_firewall_rules()
                .account(&account)
                .send()
                .await?;
            let rules = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&rules)?);
            } else {
                for rule in &rules {
                    println!("{}: {}", rule.id, rule.rule);
                }
            }
        }
        Commands::ListServices { raw } => {
            let resp = client
                .inner()
                .list_services()
                .account(&account)
                .send()
                .await?;
            let services = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&services)?);
            } else {
                for (name, url) in &services {
                    println!("{}: {}", name, url);
                }
            }
        }
        Commands::ListDatacenters { raw } => {
            let resp = client
                .inner()
                .list_datacenters()
                .account(&account)
                .send()
                .await?;
            let dcs = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&dcs)?);
            } else {
                for (name, url) in &dcs {
                    println!("{}: {}", name, url);
                }
            }
        }
        Commands::TestDocs => {
            // Handled above before authentication setup
            unreachable!()
        }
    }

    Ok(())
}

/// Test documentation redirect endpoints
///
/// These endpoints cannot be included in the Dropshot API trait due to routing
/// conflicts with `/{account}`. They must be handled at the reverse proxy or
/// HTTP server level. This function tests that they are properly configured.
async fn test_docs(base_url: &str) -> Result<()> {
    use reqwest::redirect::Policy;

    // Build a client that doesn't follow redirects
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()?;

    println!("Testing documentation redirects...");
    println!("Note: These endpoints must be handled at the reverse proxy level,");
    println!("      not by the Dropshot API, due to routing conflicts.");
    println!();

    let tests = [
        ("/", DOCS_URL),
        ("/docs", DOCS_URL),
        ("/favicon.ico", FAVICON_URL),
    ];

    let mut failures = 0;

    for (path, expected_location) in tests {
        let url = format!("{}{}", base_url.trim_end_matches('/'), path);
        let result = client.get(&url).send().await;

        match result {
            Ok(response) => {
                let status = response.status().as_u16();
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                if status == 302 && location == expected_location {
                    println!("  GET {} -> {} Location: {} [OK]", path, status, location);
                } else if status != 302 {
                    println!("  GET {} -> {} [FAIL: expected 302]", path, status);
                    failures += 1;
                } else {
                    println!(
                        "  GET {} -> {} Location: {} [FAIL: expected {}]",
                        path, status, location, expected_location
                    );
                    failures += 1;
                }
            }
            Err(e) => {
                println!("  GET {} -> [ERROR: {}]", path, e);
                failures += 1;
            }
        }
    }

    if failures == 0 {
        println!("All documentation endpoints working correctly.");
        Ok(())
    } else {
        anyhow::bail!("{} of {} tests failed.", failures, tests.len());
    }
}
