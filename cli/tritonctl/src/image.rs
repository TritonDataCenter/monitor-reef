// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonctl image` — read-only, tenant-scoped image catalog.
//!
//! Lists and shows images the caller can see (the unioned Public + Silo
//! + Tenant view). The tenant is inferred from the bearer token; we only
//! narrow by `--project`. This never touches the operator scope helpers
//! (`--silo`/`--tenant`); registering images is `tritonadm`'s job.

use anyhow::{Context, Result};
use clap::Subcommand;
use triton_cli_core::{Table, emit};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum ImageCmd {
    /// List images visible to you.
    List,
    /// Show one image by UUID.
    Show { id: Uuid },
}

pub async fn run(
    cli: &crate::Cli,
    cmd: &ImageCmd,
    format: triton_cli_core::OutputFormat,
) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        ImageCmd::List => {
            // Tenant is inferred from the token; `scope=tenant` selects
            // the unioned Public + Silo + Tenant visibility view. We only
            // ever narrow by project, and never set `tenant` or `silo`.
            let mut req = client
                .list_images_v1()
                .scope(tritond_client::types::ImageScopeSelector::Tenant);
            if let Some(p) = cli.project {
                req = req.project(p);
            }
            let page = req.send().await.context("list images")?.into_inner();
            if emit(format, &page)? {
                return Ok(());
            }
            let mut t = Table::new(&["ID", "NAME", "OS", "VERSION"], cli.no_headers);
            for img in &page.items {
                t.row([
                    img.id.to_string(),
                    img.name.clone(),
                    img.os.clone(),
                    img.version.clone(),
                ]);
            }
            t.print();
            Ok(())
        }
        ImageCmd::Show { id } => {
            let img = client
                .get_image_v1()
                .image_id(*id)
                .send()
                .await
                .context("get image")?
                .into_inner();
            if emit(format, &img)? {
                return Ok(());
            }
            print_image(&img);
            Ok(())
        }
    }
}

fn print_image(img: &tritond_client::types::Image) {
    println!("id:      {}", img.id);
    println!("name:    {}", img.name);
    println!("os:      {}", img.os);
    println!("version: {}", img.version);
    println!("size:    {} bytes", img.size_bytes);
    println!("sha256:  {}", img.sha256);
    println!("(use -o json for the full record)");
}
