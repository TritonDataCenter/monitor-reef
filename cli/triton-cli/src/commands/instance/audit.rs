// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance audit command

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

use crate::output::{json, table};

#[derive(Args, Clone)]
pub struct AuditArgs {
    /// Instance ID or name
    pub instance: String,

    /// Maximum results
    #[arg(long)]
    pub limit: Option<i64>,
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
        let mut tbl = table::create_table(&["TIME", "ACTION", "SUCCESS", "CALLER"]);

        for audit in &audits {
            let time = &audit.time;
            let action = &audit.action;
            let success = audit
                .success
                .map(|s| if s { "yes" } else { "no" })
                .unwrap_or("-");
            let caller = "-"; // caller is optional complex type

            tbl.add_row(vec![time, action, success, caller]);
        }

        table::print_table(tbl);
    }

    Ok(())
}
