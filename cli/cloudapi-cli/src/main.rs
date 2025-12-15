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
    }

    Ok(())
}
