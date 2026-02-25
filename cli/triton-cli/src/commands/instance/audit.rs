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
use crate::output::table::{TableBuilder, TableFormatArgs};

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
    let account = &client.auth_config().account;

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
        let short_cols = ["time", "action", "success"];
        let long_cols = ["caller"];

        let mut tbl =
            TableBuilder::new(&["TIME", "ACTION", "SUCCESS"]).with_long_headers(&["CALLER"]);

        let all_cols: Vec<&str> = short_cols.iter().chain(long_cols.iter()).copied().collect();
        for audit in &audits {
            let row = all_cols
                .iter()
                .map(|col| get_audit_field_value(audit, col))
                .collect();
            tbl.add_row(row);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

fn get_audit_field_value(audit: &AuditEntry, field: &str) -> String {
    match field.to_lowercase().as_str() {
        "time" => audit.time.clone(),
        "action" => audit.action.clone(),
        "success" => audit
            .success
            .map(|s| if s { "yes" } else { "no" })
            .unwrap_or("-")
            .to_string(),
        "caller" => audit
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
            .unwrap_or_else(|| "-".to_string()),
        _ => "-".to_string(),
    }
}
