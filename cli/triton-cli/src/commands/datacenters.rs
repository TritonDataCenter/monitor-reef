// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Datacenters command

use anyhow::Result;
use cloudapi_client::TypedClient;

use crate::output::{json, table};

/// List datacenters
pub async fn run(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
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
        let mut tbl = table::create_table(&["NAME", "URL"]);
        // Sort by name for consistent output
        let mut entries: Vec<_> = datacenters.iter().collect();
        entries.sort_by_key(|(name, _)| *name);
        for (name, url) in entries {
            tbl.add_row(vec![name, url]);
        }
        table::print_table(tbl);
    }

    Ok(())
}
