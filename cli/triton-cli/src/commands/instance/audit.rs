// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance audit command

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use cloudapi_client::types::AuditEntry;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs, col};

#[derive(Args, Clone)]
pub struct AuditArgs {
    /// Instance ID or name
    pub instance: String,

    /// Maximum results
    #[arg(long)]
    pub limit: Option<i64>,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

pub async fn run(args: AuditArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let response = client
        .inner()
        .machine_audit()
        .account(account)
        .machine(machine_id)
        .send()
        .await?;

    let audits = response.into_inner();

    if use_json {
        json::print_json_stream(&audits)?;
    } else {
        let columns = vec![
            col("TIME", |audit: &AuditEntry| audit.time.clone()),
            col("ACTION", |audit: &AuditEntry| audit.action.clone()),
            col("SUCCESS", |audit: &AuditEntry| {
                audit
                    .success
                    .map(|s| if s { "yes" } else { "no" })
                    .unwrap_or("-")
                    .to_string()
            }),
            // long-only columns (from index 3)
            col("CALLER", |audit: &AuditEntry| {
                audit
                    .caller
                    .as_ref()
                    .map(|c| {
                        if let Some(obj) = c.as_object()
                            && let Some(login) = obj.get("login").and_then(|v| v.as_str())
                        {
                            return login.to_string();
                        }
                        c.to_string()
                    })
                    .unwrap_or_else(|| "-".to_string())
            }),
        ];

        TableBuilder::from_columns(&columns, &audits, Some(3)).print(&args.table);
    }

    Ok(())
}
