// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! CloudAPI CLI - Command-line interface for Triton CloudAPI
//!
//! This CLI provides access to all CloudAPI endpoints for managing virtual machines,
//! images, networks, volumes, and other resources in Triton.

use anyhow::Result;
use clap::{Parser, Subcommand};
use cloudapi_api::{DOCS_URL, FAVICON_URL};
use cloudapi_client::TypedClient;

#[derive(Parser)]
#[command(name = "cloudapi", version, about = "CLI for Triton CloudAPI")]
struct Cli {
    #[arg(
        long,
        env = "CLOUDAPI_URL",
        default_value = "https://cloudapi.tritondatacenter.com"
    )]
    base_url: String,

    #[arg(long, env = "CLOUDAPI_ACCOUNT")]
    account: Option<String>,

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

fn require_account(account: Option<String>) -> Result<String> {
    account.ok_or_else(|| {
        anyhow::anyhow!(
            "Account required. Set via --account flag or CLOUDAPI_ACCOUNT environment variable"
        )
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = TypedClient::new(&cli.base_url);

    match cli.command {
        Commands::GetAccount { raw } => {
            let account = require_account(cli.account)?;
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
            let account = require_account(cli.account)?;
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
                    println!(
                        "{}: {} ({})",
                        m.id,
                        m.name.as_deref().unwrap_or("unnamed"),
                        m.state
                    );
                }
            }
        }
        Commands::GetMachine { machine, raw } => {
            let account = require_account(cli.account)?;
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
                    m.id,
                    m.name.as_deref().unwrap_or("unnamed"),
                    m.state,
                    m.image
                );
            }
        }
        Commands::StartMachine { machine } => {
            let account = require_account(cli.account)?;
            client.start_machine(&account, &machine, None).await?;
            println!("Machine started");
        }
        Commands::StopMachine { machine } => {
            let account = require_account(cli.account)?;
            client.stop_machine(&account, &machine, None).await?;
            println!("Machine stopped");
        }
        Commands::ListImages { raw } => {
            let account = require_account(cli.account)?;
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
                    let version = img.version.as_deref().unwrap_or("unknown");
                    println!("{}: {} {} ({})", img.id, img.name, version, img.state);
                }
            }
        }
        Commands::GetImage { image, raw } => {
            let account = require_account(cli.account)?;
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
                println!(
                    "ID: {}\nName: {}\nVersion: {}\nState: {}",
                    img.id,
                    img.name,
                    img.version.as_deref().unwrap_or("unknown"),
                    img.state
                );
            }
        }
        Commands::ListPackages { raw } => {
            let account = require_account(cli.account)?;
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
            let account = require_account(cli.account)?;
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
            let account = require_account(cli.account)?;
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
            let account = require_account(cli.account)?;
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
            let account = require_account(cli.account)?;
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
                for svc in &services {
                    println!("{}: {}", svc.name, svc.endpoint);
                }
            }
        }
        Commands::ListDatacenters { raw } => {
            let account = require_account(cli.account)?;
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
                for dc in &dcs {
                    println!("{}: {}", dc.name, dc.url);
                }
            }
        }
        Commands::TestDocs => {
            test_docs(&cli.base_url).await?;
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
