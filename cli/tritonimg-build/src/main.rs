// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonimg-build` — package a SmartOS dataset as a tritond
//! image bundle.
//!
//! Replaces the per-host `imgadm create` step. Operators run
//! this on a host that has the source dataset to produce a
//! `bundle.tar` containing `manifest.json` + `content.zfs.gz`.
//! The bundle is then served somewhere HTTP-reachable from
//! every CN, and tritond is told about it via
//! `POST /v2/silos/.../images { "bundle_url": "..." }`.
//!
//! ## What it does, step by step
//!
//! 1. Spawn `zfs send <ds>@<snap>` and pipe stdout into
//!    `gzip -c`, writing to a temp file under the output
//!    directory.
//! 2. SHA-256 the gzipped bytes as they're written (one pass,
//!    no second read).
//! 3. Build the tritond image manifest from CLI args + the
//!    computed sha256/size.
//! 4. Tar the manifest + content into the named output bundle.
//!
//! ## Why we shell out to `zfs send | gzip`
//!
//! Same reason `tritonagent::zfs::recv_gzipped` does: piping
//! between two child processes from a Rust async path is more
//! ceremony than `sh -c "gzip pipefail"` justifies for this
//! one-shot CLI. Both tools sit on illumos; a future cross-
//! platform port (build bundles on Linux from a remote zfs
//! source) would replace this with the tokio-process pipe
//! pattern.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use sha2::{Digest, Sha256};
use tracing::info;
use tracing_subscriber::EnvFilter;
use tritond_image_manifest::{
    Compatibility, Content, Guest, Manifest, SCHEMA_V1, content_format, write_bundle,
};

#[derive(Debug, Parser)]
#[command(
    disable_version_flag = true,
    about = "Package a SmartOS dataset as a tritond image bundle"
)]
struct Cli {
    /// ZFS snapshot to package, e.g.
    /// `zones/a7199134-...@final`. Must already exist.
    #[arg(long)]
    zfs_source: String,

    /// Operator-friendly name for the image. Surfaced in
    /// `tcadm silo image list`.
    #[arg(long)]
    name: String,

    /// Build version / release tag (e.g. `20240612.1`).
    #[arg(long)]
    version: String,

    /// SmartOS brand the image is built for (e.g.
    /// `joyent-minimal`). Agent rejects a Provision whose
    /// instance brand doesn't match.
    #[arg(long, default_value = "joyent-minimal")]
    brand: String,

    /// CPU architecture. Default `x86_64`.
    #[arg(long, default_value = "x86_64")]
    arch: String,

    /// Optional `min_smartos_platform` buildstamp
    /// (`YYYYMMDDTHHMMSSZ`). When set, the agent rejects
    /// Provision on a host whose platform is older than this.
    #[arg(long)]
    min_smartos_platform: Option<String>,

    /// Guest OS family (e.g. `linux`, `smartos`). Free-form;
    /// surfaced in operator UI.
    #[arg(long)]
    os_family: String,

    /// Guest OS version (e.g. `ubuntu-24.04`, `21.4.0`).
    /// Free-form.
    #[arg(long)]
    os_version: String,

    /// Optional human-readable description.
    #[arg(long)]
    description: Option<String>,

    /// Account names mdata-fetch should expect inside the
    /// guest (e.g. `--default-user root --default-user admin`).
    #[arg(long = "default-user")]
    default_users: Vec<String>,

    /// Output bundle path. Defaults to `<name>-<version>.tar`
    /// in the current directory.
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    let output = cli
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{}-{}.tar", cli.name, cli.version)));
    let work_dir = output
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let content_path = work_dir.join(format!("{}-{}.zfs.gz", cli.name, cli.version));

    info!(
        zfs_source = %cli.zfs_source,
        content_path = %content_path.display(),
        "running zfs send | gzip",
    );
    let (sha256, size) = stream_zfs_send_gzipped(&cli.zfs_source, &content_path)?;
    info!(sha256, size, "content materialised");

    let manifest = Manifest {
        schema: SCHEMA_V1.to_string(),
        name: cli.name.clone(),
        version: cli.version.clone(),
        description: cli.description,
        content: Content {
            format: content_format::ZFS_SEND_GZ.to_string(),
            sha256,
            size,
        },
        compatibility: Compatibility {
            brand: cli.brand,
            arch: cli.arch,
            min_smartos_platform: cli.min_smartos_platform,
        },
        guest: Guest {
            os_family: cli.os_family,
            os_version: cli.os_version,
            default_users: cli.default_users,
        },
    };

    info!(output = %output.display(), "writing bundle");
    write_bundle(&output, &manifest, &content_path).context("write bundle tar")?;

    // Cleanup intermediate. The bundle is now self-contained.
    if let Err(e) = std::fs::remove_file(&content_path) {
        tracing::warn!(
            path = %content_path.display(),
            error = %e,
            "could not remove intermediate content file; bundle is fine",
        );
    }

    println!("{}", output.display());
    Ok(())
}

/// Spawn `zfs send <source> | gzip -c > <dest>` via
/// `/bin/sh -c "set -o pipefail; …"` (so a `zfs send` failure
/// surfaces, not just the gzip exit), then read `<dest>` once
/// to compute sha256 + size. Returns `(sha256_hex, size)`.
///
/// Reading the file twice (once via the pipeline write, then
/// again to hash) costs a stat + a sequential read; not worth
/// the complexity of teeing through Rust given this is a
/// one-shot operator command.
fn stream_zfs_send_gzipped(zfs_source: &str, dest: &std::path::Path) -> Result<(String, u64)> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 destination path: {dest:?}"))?;
    let pipeline = format!("set -o pipefail; zfs send {zfs_source} | gzip -c > {dest_str}",);
    let status = Command::new("/bin/sh")
        .args(["-c", &pipeline])
        .stdin(Stdio::null())
        .status()
        .context("spawn /bin/sh -c 'zfs send | gzip'")?;
    if !status.success() {
        bail!("zfs send | gzip failed with {status}");
    }
    let mut file = std::fs::File::open(dest).context("open gzipped content for hashing")?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf).context("read gzipped content")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    // Make sure stdout has flushed before main() returns.
    let _ = std::io::stdout().flush();
    Ok((format!("{:x}", hasher.finalize()), total))
}
