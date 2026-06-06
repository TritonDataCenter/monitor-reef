// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonctl disk` — the tenant-facing disk surface.
//!
//! Disks belong to instances, so list is scoped by `--instance`. The
//! tenant is inferred from the bearer token; this never targets the
//! operator `/v1/system/*` surface.

use anyhow::{Context, Result};
use clap::Subcommand;
use triton_cli_core::{OutputFormat, Table, emit};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum DiskCmd {
    /// List the disks of an instance.
    List {
        /// Instance whose disks to list.
        #[arg(long)]
        instance: Uuid,
    },
    /// Show one disk by UUID.
    Show { id: Uuid },
    /// Grow a disk's backing volume. Grow-only. A running guest sees the
    /// new capacity after a reboot (cloud-init then grows the filesystem).
    Resize {
        id: Uuid,
        /// New total disk size in bytes. Must be larger than the current
        /// size.
        #[arg(long)]
        size_bytes: u64,
    },
}

pub async fn run(cli: &crate::Cli, cmd: &DiskCmd, format: OutputFormat) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        DiskCmd::List { instance } => {
            let page = client
                .list_disks_v1()
                .instance(*instance)
                .send()
                .await
                .context("list disks")?
                .into_inner();
            if emit(format, &page)? {
                return Ok(());
            }
            let mut t = Table::new(&["ID", "NAME", "INSTANCE", "KIND", "SIZE"], cli.no_headers);
            for d in &page.items {
                t.row([
                    d.id.to_string(),
                    d.name.clone(),
                    d.instance_id.to_string(),
                    crate::wire(&d.kind),
                    d.size_bytes.to_string(),
                ]);
            }
            t.print();
            Ok(())
        }
        DiskCmd::Show { id } => {
            let disk = client
                .get_disk_v1()
                .disk_id(*id)
                .send()
                .await
                .context("get disk")?
                .into_inner();
            if emit(format, &disk)? {
                return Ok(());
            }
            print_disk(&disk);
            Ok(())
        }
        DiskCmd::Resize { id, size_bytes } => {
            let resp = client
                .resize_disk_v1()
                .disk_id(*id)
                .body(tritond_client::types::DiskResizeRequest {
                    size_bytes: *size_bytes,
                })
                .send()
                .await
                .context("resize disk")?
                .into_inner();
            if emit(format, &resp)? {
                return Ok(());
            }
            println!(
                "Disk {} resized to {} bytes.",
                resp.disk.id, resp.disk.size_bytes
            );
            if resp.reboot_required {
                println!(
                    "  reboot required: the running guest sees the new capacity after a reboot \
                     (cloud-init then grows the partition + filesystem)"
                );
            } else {
                println!("  the larger disk is available on the next start");
            }
            Ok(())
        }
    }
}

fn print_disk(disk: &tritond_client::types::Disk) {
    println!("id:         {}", disk.id);
    println!("name:       {}", disk.name);
    println!("instance:   {}", disk.instance_id);
    println!("kind:       {}", crate::wire(&disk.kind));
    println!("size_bytes: {}", disk.size_bytes);
    println!("(use -o json for the full record)");
}
