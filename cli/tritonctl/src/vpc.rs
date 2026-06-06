// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tritonctl `vpc` — tenant-scoped VPC management.
//!
//! The tenant is inferred from the bearer token; `--project` narrows
//! within the tenant. We never set `tenant` or `silo`.

use anyhow::{Context, Result};
use clap::Subcommand;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum VpcCmd {
    /// List your VPCs.
    List,
    /// Show one VPC by UUID.
    Show { id: Uuid },
    /// Create a VPC under a project.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// IPv4 CIDR block (one of ipv4-block / ipv6-block required).
        #[arg(long = "ipv4-block")]
        ipv4_block: Option<String>,
        /// IPv6 CIDR block.
        #[arg(long = "ipv6-block")]
        ipv6_block: Option<String>,
    },
    /// Delete a VPC (server enforces the dependency gate).
    Delete { id: Uuid },
}

pub async fn run(
    cli: &crate::Cli,
    cmd: &VpcCmd,
    format: triton_cli_core::OutputFormat,
) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        VpcCmd::List => {
            let mut req = client.list_vpcs_v1();
            if let Some(p) = cli.project {
                req = req.project(p);
            }
            let page = req.send().await.context("list vpcs")?.into_inner();
            if triton_cli_core::emit(format, &page)? {
                return Ok(());
            }
            let mut t = triton_cli_core::Table::new(&["ID", "NAME", "VNI"], cli.no_headers);
            for v in &page.items {
                t.row([v.id.to_string(), v.name.clone(), v.vni.to_string()]);
            }
            t.print();
            Ok(())
        }
        VpcCmd::Show { id } => {
            let v = client
                .get_vpc_v1()
                .vpc_id(*id)
                .send()
                .await
                .context("get vpc")?
                .into_inner();
            if triton_cli_core::emit(format, &v)? {
                return Ok(());
            }
            print_vpc(&v);
            Ok(())
        }
        VpcCmd::Create {
            name,
            description,
            ipv4_block,
            ipv6_block,
        } => {
            let project = cli
                .project
                .context("--project is required to create a vpc")?;
            let v = client
                .create_vpc_v1()
                .project(project)
                .body(tritond_client::types::NewVpc {
                    name: name.clone(),
                    description: description.clone(),
                    ipv4_block: ipv4_block.clone(),
                    ipv6_block: ipv6_block.clone(),
                })
                .send()
                .await
                .context("create vpc")?
                .into_inner();
            if triton_cli_core::emit(format, &v)? {
                return Ok(());
            }
            print_vpc(&v);
            Ok(())
        }
        VpcCmd::Delete { id } => {
            client
                .delete_vpc_v1()
                .vpc_id(*id)
                .send()
                .await
                .context("delete vpc")?;
            println!("Vpc {id} deleted.");
            Ok(())
        }
    }
}

fn print_vpc(v: &tritond_client::types::Vpc) {
    println!("id:      {}", v.id);
    println!("name:    {}", v.name);
    println!("vni:     {}", v.vni);
    println!("project: {}", v.project_id);
    println!("(use -o json for the full record)");
}
