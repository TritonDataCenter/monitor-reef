// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonctl nat-gateway` — tenant-scoped NAT gateway management.

use anyhow::{Context, Result};
use clap::Subcommand;
use triton_cli_core::{OutputFormat, Table, emit};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum NatGatewayCmd {
    /// List NAT gateways visible to you (optionally narrowed by VPC).
    List {
        /// Restrict to one VPC.
        #[arg(long)]
        vpc: Option<Uuid>,
    },
    /// Show one NAT gateway by UUID.
    Show { id: Uuid },
    /// Create a NAT gateway on a VPC.
    Create {
        #[arg(long)]
        vpc: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// ipv4 | ipv6
        #[arg(long, default_value = "ipv4")]
        family: String,
    },
    /// Delete a NAT gateway by UUID.
    Delete { id: Uuid },
}

pub async fn run(cli: &crate::Cli, cmd: &NatGatewayCmd, format: OutputFormat) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        NatGatewayCmd::List { vpc } => {
            // Tenant is inferred from the token; we only narrow by
            // project and VPC. We never set `tenant` or `silo`.
            let mut req = client.list_nat_gateways_v1();
            if let Some(p) = cli.project {
                req = req.project(p);
            }
            if let Some(v) = vpc {
                req = req.vpc(*v);
            }
            let page = req.send().await.context("list nat gateways")?.into_inner();
            if emit(format, &page)? {
                return Ok(());
            }
            let mut t = Table::new(&["ID", "NAME", "PUBLIC_ADDR"], cli.no_headers);
            for g in &page.items {
                t.row([
                    g.id.to_string(),
                    g.name.clone(),
                    g.public_address.to_string(),
                ]);
            }
            t.print();
            Ok(())
        }
        NatGatewayCmd::Show { id } => {
            let g = client
                .get_nat_gateway_v1()
                .nat_gateway_id(*id)
                .send()
                .await
                .context("get nat gateway")?
                .into_inner();
            if emit(format, &g)? {
                return Ok(());
            }
            println!("id:             {}", g.id);
            println!("name:           {}", g.name);
            println!("vpc:            {}", g.vpc_id);
            println!("project:        {}", g.project_id);
            println!("public_address: {}", g.public_address);
            println!("family:         {}", crate::wire(&g.family));
            Ok(())
        }
        NatGatewayCmd::Create {
            vpc,
            name,
            description,
            family,
        } => {
            let family = match family.to_ascii_lowercase().as_str() {
                "ipv4" | "v4" => tritond_client::types::AddressFamily::V4,
                "ipv6" | "v6" => tritond_client::types::AddressFamily::V6,
                other => anyhow::bail!("unknown family `{other}`; expected ipv4 or ipv6"),
            };
            let g = client
                .create_nat_gateway_v1()
                .vpc(*vpc)
                .body(tritond_client::types::NewNatGateway {
                    name: name.clone(),
                    description: Some(description.clone()),
                    family,
                })
                .send()
                .await
                .context("create nat gateway")?
                .into_inner();
            if emit(format, &g)? {
                return Ok(());
            }
            println!("Created NAT gateway {}", g.id);
            println!("name:           {}", g.name);
            println!("vpc:            {}", g.vpc_id);
            println!("public_address: {}", g.public_address);
            Ok(())
        }
        NatGatewayCmd::Delete { id } => {
            client
                .delete_nat_gateway_v1()
                .nat_gateway_id(*id)
                .send()
                .await
                .context("delete nat gateway")?;
            println!("NAT gateway {id} deleted.");
            Ok(())
        }
    }
}
