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
use crate::output::table::{TableBuilder, TableFormatArgs, col};

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

        let columns = vec![
            col("NAME", |(name, _url): &(&String, &String)| name.to_string()),
            col("URL", |(_name, url): &(&String, &String)| url.to_string()),
        ];

        TableBuilder::from_columns(&columns, &entries, None).print(&args.table)?;
    }

    Ok(())
}
