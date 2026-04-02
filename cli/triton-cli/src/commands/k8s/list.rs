// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! List clusters

use anyhow::Result;
use clap::Args;

use super::state::{ClusterState, list_clusters};
use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs, col};

#[derive(Args, Clone)]
pub struct ListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

pub async fn run(args: ListArgs, use_json: bool) -> Result<()> {
    let clusters = list_clusters().await?;

    if use_json {
        json::print_json(&clusters)?;
    } else {
        print_clusters_table(&clusters, &args)?;
    }

    Ok(())
}

fn print_clusters_table(clusters: &[ClusterState], args: &ListArgs) -> Result<()> {
    let columns = vec![
        col("SHORTID", |c: &&ClusterState| {
            let id_str = c.uuid.to_string();
            id_str[..8.min(id_str.len())].to_string()
        }),
        col("NAME", |c: &&ClusterState| c.name.clone()),
        col("CREATED", |c: &&ClusterState| {
            c.created_at.format("%Y-%m-%d %H:%M").to_string()
        }),
        col("NODES", |c: &&ClusterState| c.nodes.len().to_string()),
        col("ENDPOINT", |c: &&ClusterState| {
            c.control_plane
                .as_ref()
                .and_then(|cp| cp.endpoint.clone())
                .unwrap_or_else(|| "-".to_string())
        }),
        col("ID", |c: &&ClusterState| c.uuid.to_string()),
        col("DESCRIPTION", |c: &&ClusterState| {
            c.description.clone().unwrap_or_else(|| "-".to_string())
        }),
    ];

    let mut table_opts = args.table.clone();
    if table_opts.columns.is_none() {
        table_opts.columns = Some(
            if table_opts.long {
                vec!["ID", "NAME", "CREATED", "NODES", "ENDPOINT", "DESCRIPTION"]
            } else {
                vec!["SHORTID", "NAME", "CREATED", "NODES", "ENDPOINT"]
            }
            .into_iter()
            .map(String::from)
            .collect(),
        );
    }

    TableBuilder::from_columns(&columns, &clusters.iter().collect::<Vec<_>>(), None)
        .print(&table_opts)?;

    Ok(())
}
