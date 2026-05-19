// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! List Kubernetes clusters

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::Cluster;

use crate::output::table::{TableBuilder, TableFormatArgs, col};
use crate::output::{enum_to_display, format_age, json};

#[derive(Args, Clone)]
pub struct ListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

pub async fn run(args: ListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let list = client
        .inner()
        .k8s_clusters_list()
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to list clusters: {}", e))?
        .into_inner();

    if use_json {
        json::print_json_stream(&list.items)?;
    } else {
        print_clusters_table(&list.items, &args)?;
    }

    Ok(())
}

fn print_clusters_table(clusters: &[Cluster], args: &ListArgs) -> Result<()> {
    // Default columns: SHORTID NAME STATE CP WORKERS CREATED
    // Long columns additionally: ID ENDPOINT TALOS_VERSION
    let columns = vec![
        col("SHORTID", |c: &Cluster| {
            let id = c.id.to_string();
            id[..8.min(id.len())].to_string()
        }),
        col("NAME", |c: &Cluster| c.name.clone()),
        col("STATE", |c: &Cluster| enum_to_display(&c.state)),
        col("CP", |c: &Cluster| c.control_plane_count.to_string()),
        col("WORKERS", |c: &Cluster| c.worker_count.to_string()),
        col("CREATED", |c: &Cluster| format_age(&c.created_at)),
        // Long-only columns below (long_from = 6)
        col("ID", |c: &Cluster| c.id.to_string()),
        col("ENDPOINT", |c: &Cluster| {
            c.endpoint.clone().unwrap_or_else(|| "-".to_string())
        }),
        col("TALOS_VERSION", |c: &Cluster| {
            c.talos_version.clone().unwrap_or_else(|| "-".to_string())
        }),
    ];

    // When no explicit columns are specified, select default or long set.
    let mut table_opts = args.table.clone();
    if table_opts.columns.is_none() {
        table_opts.columns = Some(
            if table_opts.long {
                vec![
                    "ID",
                    "NAME",
                    "STATE",
                    "CP",
                    "WORKERS",
                    "CREATED",
                    "ENDPOINT",
                    "TALOS_VERSION",
                ]
            } else {
                vec!["SHORTID", "NAME", "STATE", "CP", "WORKERS", "CREATED"]
            }
            .into_iter()
            .map(String::from)
            .collect(),
        );
    }

    TableBuilder::from_columns(&columns, clusters, Some(6)).print(&table_opts)?;
    Ok(())
}
