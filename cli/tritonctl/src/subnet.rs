// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tritonctl `subnet` — tenant-scoped subnet management.
//!
//! Subnets are scoped by their parent VPC (`--vpc`); the tenant is
//! inferred from the bearer token. We never set `tenant` or `silo`.

use anyhow::{Context, Result};
use clap::Subcommand;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum SubnetCmd {
    /// List subnets in a VPC.
    List {
        #[arg(long)]
        vpc: Uuid,
    },
    /// Show one subnet by UUID.
    Show { id: Uuid },
    /// Create a subnet inside a VPC.
    Create {
        #[arg(long)]
        vpc: Uuid,
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
    /// Delete a subnet (server enforces the dependency gate).
    Delete { id: Uuid },
}

pub async fn run(
    cli: &crate::Cli,
    cmd: &SubnetCmd,
    format: triton_cli_core::OutputFormat,
) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        SubnetCmd::List { vpc } => {
            let page = client
                .list_subnets_v1()
                .vpc(*vpc)
                .send()
                .await
                .context("list subnets")?
                .into_inner();
            if triton_cli_core::emit(format, &page)? {
                return Ok(());
            }
            let mut t = triton_cli_core::Table::new(&["ID", "NAME", "VPC"], cli.no_headers);
            for s in &page.items {
                t.row([s.id.to_string(), s.name.clone(), s.vpc_id.to_string()]);
            }
            t.print();
            Ok(())
        }
        SubnetCmd::Show { id } => {
            let s = client
                .get_subnet_v1()
                .subnet_id(*id)
                .send()
                .await
                .context("get subnet")?
                .into_inner();
            if triton_cli_core::emit(format, &s)? {
                return Ok(());
            }
            print_subnet(&s);
            Ok(())
        }
        SubnetCmd::Create {
            vpc,
            name,
            description,
            ipv4_block,
            ipv6_block,
        } => {
            let s = client
                .create_subnet_v1()
                .vpc(*vpc)
                .body(tritond_client::types::NewSubnet {
                    name: name.clone(),
                    description: description.clone(),
                    ipv4_block: ipv4_block.clone(),
                    ipv6_block: ipv6_block.clone(),
                })
                .send()
                .await
                .context("create subnet")?
                .into_inner();
            if triton_cli_core::emit(format, &s)? {
                return Ok(());
            }
            print_subnet(&s);
            Ok(())
        }
        SubnetCmd::Delete { id } => {
            client
                .delete_subnet_v1()
                .subnet_id(*id)
                .send()
                .await
                .context("delete subnet")?;
            println!("Subnet {id} deleted.");
            Ok(())
        }
    }
}

fn print_subnet(s: &tritond_client::types::Subnet) {
    println!("id:   {}", s.id);
    println!("name: {}", s.name);
    println!("vpc:  {}", s.vpc_id);
    println!("(use -o json for the full record)");
}
