// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonctl route` — tenant-scoped route management.

use anyhow::{Context, Result};
use clap::Subcommand;
use triton_cli_core::{OutputFormat, Table, emit};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum RouteCmd {
    /// List routes visible to you (optionally narrowed by route table).
    List {
        /// Restrict to one route table.
        #[arg(long = "route-table")]
        route_table: Option<Uuid>,
    },
    /// Show one route by UUID.
    Show { id: Uuid },
    /// Create a route in a route table. Exactly one of the `--target-*`
    /// flags must be provided.
    Create {
        #[arg(long = "route-table")]
        route_table: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// Destination CIDR.
        #[arg(long)]
        destination: String,
        /// Target: send to a NAT gateway by UUID.
        #[arg(long = "target-nat-gateway")]
        target_nat_gateway: Option<Uuid>,
        /// Target: blackhole the traffic.
        #[arg(long = "target-blackhole")]
        target_blackhole: bool,
        /// Target: ICMP-reject the traffic.
        #[arg(long = "target-reject")]
        target_reject: bool,
        /// Target: send to the VPC's virtual gateway.
        #[arg(long = "target-virtual-gateway")]
        target_virtual_gateway: bool,
    },
    /// Delete a route by UUID.
    Delete { id: Uuid },
}

pub async fn run(cli: &crate::Cli, cmd: &RouteCmd, format: OutputFormat) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        RouteCmd::List { route_table } => {
            // Tenant is inferred from the token; we only narrow by
            // project and route table. We never set `tenant` or `silo`.
            let mut req = client.list_routes_v1();
            if let Some(p) = cli.project {
                req = req.project(p);
            }
            if let Some(rt) = route_table {
                req = req.route_table(*rt);
            }
            let page = req.send().await.context("list routes")?.into_inner();
            if emit(format, &page)? {
                return Ok(());
            }
            let mut t = Table::new(&["ID", "NAME", "DEST", "TARGET"], cli.no_headers);
            for r in &page.items {
                t.row([
                    r.id.to_string(),
                    r.name.clone(),
                    r.destination.clone(),
                    crate::wire(&r.target),
                ]);
            }
            t.print();
            Ok(())
        }
        RouteCmd::Show { id } => {
            let r = client
                .get_route_v1()
                .route_id(*id)
                .send()
                .await
                .context("get route")?
                .into_inner();
            if emit(format, &r)? {
                return Ok(());
            }
            println!("id:          {}", r.id);
            println!("name:        {}", r.name);
            println!("description: {}", r.description);
            println!("vpc:         {}", r.vpc_id);
            println!("route_table: {}", r.route_table_id);
            println!("destination: {}", r.destination);
            println!("target:      {}", crate::wire(&r.target));
            Ok(())
        }
        RouteCmd::Create {
            route_table,
            name,
            description,
            destination,
            target_nat_gateway,
            target_blackhole,
            target_reject,
            target_virtual_gateway,
        } => {
            use tritond_client::types::{NewRoute, RouteTarget};

            let chosen = [
                target_nat_gateway.is_some(),
                *target_blackhole,
                *target_reject,
                *target_virtual_gateway,
            ]
            .iter()
            .filter(|b| **b)
            .count();
            if chosen != 1 {
                anyhow::bail!(
                    "exactly one of --target-nat-gateway, --target-blackhole, --target-reject, --target-virtual-gateway must be provided ({chosen} given)"
                );
            }

            let target = if let Some(id) = target_nat_gateway {
                RouteTarget::NatGateway {
                    nat_gateway_id: *id,
                }
            } else if *target_blackhole {
                RouteTarget::Blackhole
            } else if *target_reject {
                RouteTarget::Reject
            } else {
                RouteTarget::VirtualGateway
            };

            let r = client
                .create_route_v1()
                .route_table(*route_table)
                .body(NewRoute {
                    name: name.clone(),
                    description: Some(description.clone()),
                    destination: destination.clone(),
                    target,
                })
                .send()
                .await
                .context("create route")?
                .into_inner();
            if emit(format, &r)? {
                return Ok(());
            }
            println!("Created route {}", r.id);
            println!("name:        {}", r.name);
            println!("destination: {}", r.destination);
            println!("target:      {}", crate::wire(&r.target));
            Ok(())
        }
        RouteCmd::Delete { id } => {
            client
                .delete_route_v1()
                .route_id(*id)
                .send()
                .await
                .context("delete route")?;
            println!("Route {id} deleted.");
            Ok(())
        }
    }
}
