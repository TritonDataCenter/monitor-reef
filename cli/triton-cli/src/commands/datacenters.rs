// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Datacenters command

use anyhow::Result;
use clap::Args;

use crate::client::AnyClient;
use crate::define_columns;
use crate::dispatch;
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct DatacenterListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

/// List datacenters.
///
/// First Phase-4 command ported to [`AnyClient`] — the builder chain
/// is identical in both variants, but the builder *types* come from
/// separate Progenitor crates, so we pattern-match and dispatch inside
/// each arm. The response is a `HashMap<String, String>` (a `std`
/// type), which converges across variants, so the post-call rendering
/// is variant-agnostic.
pub async fn run(args: DatacenterListArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();

    let datacenters = dispatch!(client, |c| {
        c.inner()
            .list_datacenters()
            .account(account)
            .send()
            .await?
            .into_inner()
    });

    if use_json {
        json::print_json(&datacenters)?;
    } else {
        // Sort by name for consistent output
        let mut entries: Vec<(&str, &str)> = datacenters
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        entries.sort_by_key(|(name, _)| *name);

        type DcEntry<'a> = (&'a str, &'a str);
        define_columns! {
            DcColumn for DcEntry<'_> {
                Name("NAME") => |dc| dc.0.to_string(),
                Url("URL") => |dc| dc.1.to_string(),
            }
        }

        TableBuilder::from_enum_columns::<DcColumn, _>(&entries, None).print(&args.table)?;
    }

    Ok(())
}
