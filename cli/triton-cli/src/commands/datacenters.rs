// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Datacenters command

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct DatacenterListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

/// List datacenters
pub async fn run(args: DatacenterListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let response = client
        .inner()
        .list_datacenters()
        .account(account)
        .send()
        .await?;

    let datacenters = response.into_inner();

    if use_json {
        json::print_json(&datacenters)?;
    } else {
        // Sort by name for consistent output
        let mut entries: Vec<_> = datacenters.iter().collect();
        entries.sort_by_key(|(name, _)| *name);

        let short_cols = ["name", "url"];
        let mut tbl = TableBuilder::new(&["NAME", "URL"]);
        for (name, url) in &entries {
            let row = short_cols
                .iter()
                .map(|col| get_datacenter_field_value(name, url, col))
                .collect();
            tbl.add_row(row);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

fn get_datacenter_field_value(name: &str, url: &str, field: &str) -> String {
    match field.to_lowercase().as_str() {
        "name" => name.to_string(),
        "url" => url.to_string(),
        _ => "-".to_string(),
    }
}
