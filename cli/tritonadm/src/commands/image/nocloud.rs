// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonadm image fetch-nocloud` — fetch a CloudInit nocloud cloud
//! image from an upstream vendor (POC: Ubuntu) and convert it into a
//! gzipped ZFS stream + IMGAPI manifest pair.

mod manifest;
mod pipeline;
mod vendor;
mod verify;
mod zfs;

use std::path::PathBuf;

use anyhow::{Context, Result};

pub struct FetchOpts {
    pub vendor: String,
    pub release: String,
    pub output_dir: Option<PathBuf>,
    pub workdir: Option<PathBuf>,
    pub insecure_no_verify: bool,
    pub dataset: Option<String>,
    pub dry_run: bool,
}

pub async fn run(opts: FetchOpts) -> Result<()> {
    preflight()?;

    let vendor = vendor::lookup(&opts.vendor)?;
    let http = triton_tls::build_http_client(false)
        .await
        .map_err(|e| anyhow::anyhow!("build http client: {e}"))?;

    let resolved = vendor
        .resolve(&opts.release, &http)
        .await
        .with_context(|| format!("resolve {}/{}", opts.vendor, opts.release))?;

    let dataset = match opts.dataset.clone() {
        Some(d) => d,
        None => default_dataset()?,
    };

    let stub = format!("{}-{}", opts.vendor, resolved.series);
    let workdir = opts
        .workdir
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/tritonadm/nocloud/cache/{stub}")));
    let output_dir = opts
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/tritonadm/nocloud/image/{stub}")));

    if opts.dry_run {
        print_plan(&opts, &resolved, &dataset, &workdir, &output_dir);
        return Ok(());
    }

    let outputs = pipeline::run(
        resolved,
        pipeline::PipelineOptions {
            vendor: &opts.vendor,
            workdir,
            output_dir,
            zfs_dataset: dataset,
            http: &http,
            insecure_no_verify: opts.insecure_no_verify,
        },
    )
    .await?;

    println!();
    println!("Build complete.");
    println!("  Image:    {}", outputs.gz_path.display());
    println!("  Manifest: {}", outputs.manifest_path.display());
    println!("  UUID:     {}", outputs.manifest_uuid);
    println!();
    println!("To install on this SmartOS host:");
    println!(
        "  imgadm install -f {} -m {}",
        outputs.gz_path.display(),
        outputs.manifest_path.display()
    );
    Ok(())
}

fn print_plan(
    opts: &FetchOpts,
    resolved: &vendor::ResolvedImage,
    dataset: &str,
    workdir: &std::path::Path,
    output_dir: &std::path::Path,
) {
    let src_filename = resolved
        .url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("(unknown)");
    let stub = format!(
        "{}-{}-{}",
        opts.vendor, resolved.series, resolved.version
    );

    println!("Resolved upstream image:");
    println!("  Vendor:        {}", opts.vendor);
    println!("  Codename:      {}", resolved.series);
    println!("  Version:       {}", resolved.version);
    println!("  URL:           {}", resolved.url);
    println!(
        "  Format:        {}",
        match resolved.format {
            vendor::SourceFormat::Qcow2 => "qcow2",
            vendor::SourceFormat::Raw => "raw",
            vendor::SourceFormat::Xz => "xz",
        }
    );
    match &resolved.expected_sha256 {
        Some(s) => {
            println!("  SHA-256:       {s}");
            let manifest_uuid = pipeline::stable_manifest_uuid(s);
            println!("  Manifest UUID: {manifest_uuid}  (derived from sha256)");
        }
        None => {
            println!("  SHA-256:       (fetched from vendor at verify time)");
            println!("  Manifest UUID: (derived after download — vendor publishes hash separately)");
        }
    }

    println!();
    println!("Would write to:");
    println!("  Cache file:    {}", workdir.join(src_filename).display());
    println!(
        "  Image:         {}",
        output_dir.join(format!("{stub}.x86_64.zfs.gz")).display()
    );
    println!(
        "  Manifest:      {}",
        output_dir.join(format!("{stub}.json")).display()
    );

    println!();
    println!("Would create transient zvol:");
    println!("  Parent:        {dataset}");
    println!("  Child:         tritonadm-nocloud-<random-uuid>");

    println!();
    println!("Manifest fields that would be set:");
    println!("  name:          {}-{}-nocloud", opts.vendor, resolved.series);
    println!("  version:       {}", resolved.version);
    println!("  os:            {}", resolved.os);
    println!("  ssh_key req'd: {}", resolved.ssh_key);

    println!();
    println!("(--dry-run: nothing was downloaded, written, or created.)");
}

fn preflight() -> Result<()> {
    let v = std::process::Command::new("uname")
        .arg("-v")
        .output()
        .context("spawn uname -v")?;
    let v = String::from_utf8_lossy(&v.stdout);
    if !v.starts_with("joyent_") {
        anyhow::bail!(
            "tritonadm image fetch-nocloud requires SmartOS (uname -v: {})",
            v.trim()
        );
    }
    Ok(())
}

/// Default dataset for the temporary build zvol.
///
/// In an NGZ this is the delegated dataset (`zones/<zone>/data` with
/// `zoned=on`); in the GZ we drop directly under `zones`.
fn default_dataset() -> Result<String> {
    let zone = std::process::Command::new("zonename")
        .output()
        .context("spawn zonename")?;
    if !zone.status.success() {
        anyhow::bail!("zonename exited {}", zone.status);
    }
    let zone = String::from_utf8_lossy(&zone.stdout).trim().to_string();
    if zone == "global" {
        return Ok("zones".to_string());
    }
    let dataset = format!("zones/{zone}/data");
    let zoned = std::process::Command::new("zfs")
        .args(["get", "-H", "-o", "value", "zoned", &dataset])
        .output()
        .context("spawn zfs get zoned")?;
    if !zoned.status.success()
        || String::from_utf8_lossy(&zoned.stdout).trim() != "on"
    {
        anyhow::bail!(
            "delegated dataset {dataset} not available or not zoned. \
             Pass --dataset to override."
        );
    }
    Ok(dataset)
}
