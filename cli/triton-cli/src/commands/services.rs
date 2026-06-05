// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Services listing command

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;

use crate::define_columns;
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct ServiceListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

/// List available service endpoints
pub async fn run(args: ServiceListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
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
        let mut entries: Vec<(&str, &str)> = services
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        entries.sort_by_key(|(name, _)| *name);

        type SvcEntry<'a> = (&'a str, &'a str);
        define_columns! {
            SvcColumn for SvcEntry<'_> {
                Name("NAME") => |svc| svc.0.to_string(),
                Endpoint("ENDPOINT") => |svc| svc.1.to_string(),
            }
        }

        TableBuilder::from_enum_columns::<SvcColumn, _>(&entries, None).print(&args.table)?;
    }

    Ok(())
}
