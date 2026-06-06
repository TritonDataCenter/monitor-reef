// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tritonctl `firewall-rule` resource: list/show/create/delete firewall
//! rules. Tenant is inferred from the bearer token; `--project` narrows
//! within the tenant and `--vpc` targets a VPC.

use anyhow::{Context, Result};
use clap::Subcommand;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum FirewallRuleCmd {
    /// List firewall rules. Narrow by --vpc and/or --project.
    List {
        /// Restrict to a single VPC.
        #[arg(long)]
        vpc: Option<Uuid>,
    },
    /// Show one firewall rule by UUID.
    Show { id: Uuid },
    /// Create a firewall rule on a VPC.
    Create {
        #[arg(long)]
        vpc: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// allow | deny
        #[arg(long)]
        action: String,
        /// inbound | outbound
        #[arg(long)]
        direction: String,
        /// any | tcp | udp | icmp4 | icmp6
        #[arg(long, default_value = "any")]
        protocol: String,
        #[arg(long)]
        priority: u16,
        /// Source CIDR (optional; omitted means any).
        #[arg(long = "source-cidr")]
        source_cidr: Option<String>,
        /// Destination CIDR (optional; omitted means any).
        #[arg(long = "destination-cidr")]
        destination_cidr: Option<String>,
        /// Source ports as `low-high` (TCP/UDP only).
        #[arg(long = "source-ports")]
        source_ports: Option<String>,
        /// Destination ports as `low-high` (TCP/UDP only).
        #[arg(long = "destination-ports")]
        destination_ports: Option<String>,
    },
    /// Delete a firewall rule by UUID.
    Delete { id: Uuid },
}

pub async fn run(
    cli: &crate::Cli,
    cmd: &FirewallRuleCmd,
    format: triton_cli_core::OutputFormat,
) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        FirewallRuleCmd::List { vpc } => {
            // Tenant is inferred from the token; we only ever narrow by
            // project and VPC. We never set `tenant` or `silo`.
            let mut req = client.list_firewall_rules_v1();
            if let Some(v) = vpc {
                req = req.vpc(*v);
            }
            if let Some(p) = cli.project {
                req = req.project(p);
            }
            let page = req
                .send()
                .await
                .context("list firewall rules")?
                .into_inner();
            if triton_cli_core::emit(format, &page)? {
                return Ok(());
            }
            let mut t = triton_cli_core::Table::new(
                &["ID", "NAME", "DIR", "ACTION", "PRIO", "DEST"],
                cli.no_headers,
            );
            for r in &page.items {
                t.row([
                    r.id.to_string(),
                    r.name.clone(),
                    crate::wire(&r.direction),
                    crate::wire(&r.action),
                    r.priority.to_string(),
                    r.destination_cidr.clone().unwrap_or_else(|| "any".into()),
                ]);
            }
            t.print();
            Ok(())
        }
        FirewallRuleCmd::Show { id } => {
            let r = client
                .get_firewall_rule_v1()
                .firewall_rule_id(*id)
                .send()
                .await
                .context("get firewall rule")?
                .into_inner();
            if triton_cli_core::emit(format, &r)? {
                return Ok(());
            }
            print_firewall_rule(&r);
            Ok(())
        }
        FirewallRuleCmd::Create {
            vpc,
            name,
            description,
            action,
            direction,
            protocol,
            priority,
            source_cidr,
            destination_cidr,
            source_ports,
            destination_ports,
        } => {
            use tritond_client::types::{
                FirewallAction, FirewallDirection, FirewallProtocol, NewFirewallRule,
            };

            let action = match action.to_ascii_lowercase().as_str() {
                "allow" => FirewallAction::Allow,
                "deny" => FirewallAction::Deny,
                other => anyhow::bail!("unknown action `{other}`; expected allow or deny"),
            };
            let direction = match direction.to_ascii_lowercase().as_str() {
                "inbound" | "in" => FirewallDirection::Inbound,
                "outbound" | "out" => FirewallDirection::Outbound,
                other => {
                    anyhow::bail!("unknown direction `{other}`; expected inbound or outbound")
                }
            };
            let protocol = match protocol.to_ascii_lowercase().as_str() {
                "any" => FirewallProtocol::Any,
                "tcp" => FirewallProtocol::Tcp,
                "udp" => FirewallProtocol::Udp,
                "icmp4" | "icmp" => FirewallProtocol::Icmp4,
                "icmp6" => FirewallProtocol::Icmp6,
                other => anyhow::bail!("unknown protocol `{other}`"),
            };

            let source_ports = source_ports.as_deref().map(parse_port_range).transpose()?;
            let destination_ports = destination_ports
                .as_deref()
                .map(parse_port_range)
                .transpose()?;

            let r = client
                .create_firewall_rule_v1()
                .vpc(*vpc)
                .body(NewFirewallRule {
                    name: name.clone(),
                    description: Some(description.clone()),
                    action,
                    direction,
                    protocol,
                    priority: *priority,
                    source_cidr: source_cidr.clone(),
                    destination_cidr: destination_cidr.clone(),
                    source_ports,
                    destination_ports,
                    icmp_type_code: None,
                })
                .send()
                .await
                .context("create firewall rule")?
                .into_inner();
            if triton_cli_core::emit(format, &r)? {
                return Ok(());
            }
            print_firewall_rule(&r);
            Ok(())
        }
        FirewallRuleCmd::Delete { id } => {
            client
                .delete_firewall_rule_v1()
                .firewall_rule_id(*id)
                .send()
                .await
                .context("delete firewall rule")?;
            println!("FirewallRule {id} deleted.");
            Ok(())
        }
    }
}

fn parse_port_range(s: &str) -> Result<tritond_client::types::FirewallPortRange> {
    // Accept "low-high" or a single "n" (treated as low=high=n).
    let (low_s, high_s) = match s.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (s.trim(), s.trim()),
    };
    let low: u16 = low_s
        .parse()
        .with_context(|| format!("port range low `{low_s}` is not a u16"))?;
    let high: u16 = high_s
        .parse()
        .with_context(|| format!("port range high `{high_s}` is not a u16"))?;
    if low > high {
        anyhow::bail!("port range low ({low}) > high ({high})");
    }
    Ok(tritond_client::types::FirewallPortRange { low, high })
}

fn print_firewall_rule(r: &tritond_client::types::FirewallRule) {
    println!("id:          {}", r.id);
    println!("name:        {}", r.name);
    println!("description: {}", r.description);
    println!("vpc:         {}", r.vpc_id);
    println!("project:     {}", r.project_id);
    println!("direction:   {}", crate::wire(&r.direction));
    println!("action:      {}", crate::wire(&r.action));
    println!("protocol:    {}", crate::wire(&r.protocol));
    println!("priority:    {}", r.priority);
    println!("source:      {}", r.source_cidr.as_deref().unwrap_or("any"));
    println!(
        "destination: {}",
        r.destination_cidr.as_deref().unwrap_or("any")
    );
    println!("(use -o json for the full record)");
}
