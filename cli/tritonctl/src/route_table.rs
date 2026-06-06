// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonctl route-table` — tenant-scoped route table management.

use anyhow::{Context, Result};
use clap::Subcommand;
use triton_cli_core::{OutputFormat, Table, emit};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum RouteTableCmd {
    /// List route tables visible to you (optionally narrowed by VPC).
    List {
        /// Restrict to one VPC.
        #[arg(long)]
        vpc: Option<Uuid>,
    },
    /// Show one route table by UUID.
    Show { id: Uuid },
    /// Create a non-main route table inside a VPC.
    Create {
        #[arg(long)]
        vpc: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// Delete a route table by UUID.
    Delete { id: Uuid },
}

pub async fn run(cli: &crate::Cli, cmd: &RouteTableCmd, format: OutputFormat) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        RouteTableCmd::List { vpc } => {
            // Tenant is inferred from the token; we only narrow by
            // project and VPC. We never set `tenant` or `silo`.
            let mut req = client.list_route_tables_v1();
            if let Some(p) = cli.project {
                req = req.project(p);
            }
            if let Some(v) = vpc {
                req = req.vpc(*v);
            }
            let page = req.send().await.context("list route tables")?.into_inner();
            if emit(format, &page)? {
                return Ok(());
            }
            let mut t = Table::new(&["ID", "NAME", "MAIN"], cli.no_headers);
            for rt in &page.items {
                t.row([
                    rt.id.to_string(),
                    rt.name.clone(),
                    if rt.is_main { "yes" } else { "no" }.to_string(),
                ]);
            }
            t.print();
            Ok(())
        }
        RouteTableCmd::Show { id } => {
            let rt = client
                .get_route_table_v1()
                .route_table_id(*id)
                .send()
                .await
                .context("get route table")?
                .into_inner();
            if emit(format, &rt)? {
                return Ok(());
            }
            println!("id:      {}", rt.id);
            println!("name:    {}", rt.name);
            println!("vpc:     {}", rt.vpc_id);
            println!("project: {}", rt.project_id);
            println!("is_main: {}", rt.is_main);
            Ok(())
        }
        RouteTableCmd::Create {
            vpc,
            name,
            description,
        } => {
            let rt = client
                .create_route_table_v1()
                .vpc(*vpc)
                .body(tritond_client::types::NewRouteTable {
                    name: name.clone(),
                    description: Some(description.clone()),
                })
                .send()
                .await
                .context("create route table")?
                .into_inner();
            if emit(format, &rt)? {
                return Ok(());
            }
            println!("Created route table {}", rt.id);
            println!("name: {}", rt.name);
            println!("vpc:  {}", rt.vpc_id);
            Ok(())
        }
        RouteTableCmd::Delete { id } => {
            client
                .delete_route_table_v1()
                .route_table_id(*id)
                .send()
                .await
                .context("delete route table")?;
            println!("Route table {id} deleted.");
            Ok(())
        }
    }
}
