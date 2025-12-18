// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Services listing command

use anyhow::Result;
use cloudapi_client::TypedClient;

use crate::output::{json, table};

/// List available service endpoints
pub async fn run(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_services()
        .account(account)
        .send()
        .await?;

    let services = response.into_inner();

    if use_json {
        json::print_json(&services)?;
    } else {
        // Sort by name for consistent output (matching node-triton)
        let mut entries: Vec<_> = services.iter().collect();
        entries.sort_by_key(|(name, _)| name.as_str());

        let mut tbl = table::create_table(&["NAME", "ENDPOINT"]);
        for (name, url) in entries {
            tbl.add_row(vec![name, url]);
        }
        table::print_table(tbl);
    }

    Ok(())
}
