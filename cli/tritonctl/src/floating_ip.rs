// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonctl floating-ip` — tenant-scoped floating IP management.
//!
//! Tenant is inferred from the bearer token; `--project` narrows within
//! the tenant. We never set `tenant`/`silo` selectors. Attach/detach use
//! the realized project-scoped saga route (FipClaim/FipRelease), reading
//! the path-required tenant/project from the token-scoped FIP record.

use anyhow::{Context, Result};
use clap::Subcommand;
use triton_cli_core::OutputFormat;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum FloatingIpCmd {
    /// List your floating IPs.
    List,
    /// Show one floating IP by UUID.
    Show { id: Uuid },
    /// Allocate a new floating IP under a project.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// Address family to allocate from: ipv4 or ipv6.
        #[arg(long, default_value = "ipv4")]
        family: String,
    },
    /// Delete a floating IP (must already be detached).
    Delete { id: Uuid },
    /// Attach a floating IP to a NIC.
    Attach {
        id: Uuid,
        #[arg(long)]
        nic: Uuid,
    },
    /// Detach a floating IP from its current NIC.
    Detach { id: Uuid },
}

pub async fn run(cli: &crate::Cli, cmd: &FloatingIpCmd, format: OutputFormat) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        FloatingIpCmd::List => {
            // Tenant is inferred from the token; we only ever narrow by
            // project. We never set `tenant` or `silo`.
            let mut req = client.list_floating_ips_v1();
            if let Some(p) = cli.project {
                req = req.project(p);
            }
            let page = req.send().await.context("list floating IPs")?.into_inner();
            if triton_cli_core::emit(format, &page)? {
                return Ok(());
            }
            let mut t = triton_cli_core::Table::new(
                &["ID", "NAME", "ADDRESS", "ATTACHED_TO"],
                cli.no_headers,
            );
            for fip in &page.items {
                t.row([
                    fip.id.to_string(),
                    fip.name.clone(),
                    fip.address.to_string(),
                    attached_to(fip),
                ]);
            }
            t.print();
            Ok(())
        }
        FloatingIpCmd::Show { id } => {
            let fip = client
                .get_floating_ip_v1()
                .floating_ip_id(*id)
                .send()
                .await
                .context("get floating IP")?
                .into_inner();
            if triton_cli_core::emit(format, &fip)? {
                return Ok(());
            }
            print_floating_ip(&fip);
            Ok(())
        }
        FloatingIpCmd::Create {
            name,
            description,
            family,
        } => {
            let project = cli
                .project
                .context("--project is required to create a floating IP")?;
            let family = match family.as_str() {
                "ipv4" | "v4" => tritond_client::types::AddressFamily::V4,
                "ipv6" | "v6" => tritond_client::types::AddressFamily::V6,
                other => anyhow::bail!("unknown family {other}; expected ipv4 or ipv6"),
            };
            let fip = client
                .create_floating_ip_v1()
                .project(project)
                .body(tritond_client::types::NewFloatingIp {
                    name: name.clone(),
                    description: description.clone(),
                    family: Some(family),
                    network_id: None,
                    pool_id: None,
                })
                .send()
                .await
                .context("create floating IP")?
                .into_inner();
            if triton_cli_core::emit(format, &fip)? {
                return Ok(());
            }
            print_floating_ip(&fip);
            Ok(())
        }
        FloatingIpCmd::Delete { id } => {
            client
                .delete_floating_ip_v1()
                .floating_ip_id(*id)
                .send()
                .await
                .context("delete floating IP")?;
            println!("Floating IP {id} deleted.");
            Ok(())
        }
        FloatingIpCmd::Attach { id, nic } => {
            // The realized attach is project-scoped: it runs the FipClaim
            // saga so the FIP lands on a CN. Look up the FIP (token-scoped)
            // to learn the tenant/project path params, then drive it.
            let fip = client
                .get_floating_ip_v1()
                .floating_ip_id(*id)
                .send()
                .await
                .context("floating IP lookup")?
                .into_inner();
            let fip = client
                .attach_project_floating_ip()
                .tenant_id(fip.tenant_id)
                .project_id(fip.project_id)
                .floating_ip_id(*id)
                .body(tritond_client::types::AttachFloatingIpRequest { nic_id: *nic })
                .send()
                .await
                .context("attach floating IP")?
                .into_inner();
            if triton_cli_core::emit(format, &fip)? {
                return Ok(());
            }
            println!(
                "Floating IP {} attached to NIC {} (address {}).",
                fip.id, nic, fip.address
            );
            Ok(())
        }
        FloatingIpCmd::Detach { id } => {
            // Project-scoped realized detach: runs the FipRelease saga so
            // hosted_cn is cleared on the CN. Look up the FIP for its
            // tenant/project path params first.
            let fip = client
                .get_floating_ip_v1()
                .floating_ip_id(*id)
                .send()
                .await
                .context("floating IP lookup")?
                .into_inner();
            let fip = client
                .detach_project_floating_ip()
                .tenant_id(fip.tenant_id)
                .project_id(fip.project_id)
                .floating_ip_id(*id)
                .send()
                .await
                .context("detach floating IP")?
                .into_inner();
            if triton_cli_core::emit(format, &fip)? {
                return Ok(());
            }
            println!("Floating IP {} detached (address {}).", fip.id, fip.address);
            Ok(())
        }
    }
}

fn attached_to(fip: &tritond_client::types::FloatingIp) -> String {
    match &fip.attached_to {
        Some(a) => format!("nic={}", a.nic_id),
        None => "-".to_string(),
    }
}

fn print_floating_ip(fip: &tritond_client::types::FloatingIp) {
    println!("id:          {}", fip.id);
    println!("name:        {}", fip.name);
    println!("address:     {}", fip.address);
    println!("attached_to: {}", attached_to(fip));
    println!("(use -o json for the full record)");
}
