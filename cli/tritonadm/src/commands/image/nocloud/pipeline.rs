// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Translation of `target/triton-nocloud-images/build.sh` into Rust:
//! download → verify → open qcow2 in-process → create zvol of the
//! virtual disk's size → stream qcow2 reader into the zvol's char
//! device → snap → send → gzip → manifest. No qemu-img dependency;
//! qcow2 decoding lives in the `qcow` crate.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::manifest::{self, ManifestInputs};
use super::vendor::{ResolvedImage, SourceFormat};
use super::verify;
use super::zfs;

/// UUID v5 namespace for tritonadm-generated nocloud images. Stable
/// forever — derived from a stable URL via `NAMESPACE_URL`. Manifest
/// UUIDs are then `v5(NAMESPACE, source_image_sha256_hex)`, so two
/// runs against the same upstream image always produce the same
/// manifest UUID, regardless of when or where the build runs.
fn manifest_namespace() -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        b"https://tritondatacenter.com/tritonadm/nocloud",
    )
}

fn stable_manifest_uuid(source_sha256_hex: &str) -> Uuid {
    Uuid::new_v5(&manifest_namespace(), source_sha256_hex.as_bytes())
}

pub struct PipelineOptions<'a> {
    pub vendor: &'a str,
    pub workdir: PathBuf,
    pub output_dir: PathBuf,
    pub zfs_dataset: String,
    pub http: &'a reqwest::Client,
    pub insecure_no_verify: bool,
}

pub struct PipelineOutputs {
    pub gz_path: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_uuid: Uuid,
}

pub async fn run(
    resolved: ResolvedImage,
    opts: PipelineOptions<'_>,
) -> Result<PipelineOutputs> {
    tokio::fs::create_dir_all(&opts.workdir).await?;
    tokio::fs::create_dir_all(&opts.output_dir).await?;

    let src_filename = resolved
        .url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("cannot derive filename from {}", resolved.url))?;
    let downloaded = opts.workdir.join(&src_filename);

    if tokio::fs::try_exists(&downloaded).await? {
        eprintln!("Source image already downloaded: {}", src_filename);
    } else {
        eprintln!("Downloading {}", src_filename);
        eprintln!("  URL: {}", resolved.url);
        download_with_progress(opts.http, resolved.url.as_str(), &downloaded).await?;
    }

    // Hash once. The pipeline needs this for verification AND for
    // deriving a stable manifest UUID; the Verifier trait takes a
    // precomputed hex string so we don't double-hash a 600 MB file.
    eprintln!("Hashing source image ...");
    let source_sha256 = verify::sha256_file(&downloaded).await?;

    if opts.insecure_no_verify {
        eprintln!("WARNING: --insecure-no-verify, skipping verification");
    } else {
        resolved
            .verifier
            .verify(&source_sha256, opts.http)
            .await
            .context("verification failed")?;
    }

    let virtual_size_bytes = read_virtual_size(&downloaded, resolved.format).await?;
    let virtual_size_mib = virtual_size_bytes.div_ceil(1024 * 1024);

    let build_uuid = Uuid::new_v4();
    let dataset = format!("{}/{}", opts.zfs_dataset, build_uuid);
    let zvol_rdsk = PathBuf::from(format!("/dev/zvol/rdsk/{}", dataset));

    eprintln!(
        "Creating zvol: {} ({} MiB virtual)",
        dataset, virtual_size_mib
    );
    let result = build_image(
        &resolved,
        &downloaded,
        &source_sha256,
        virtual_size_bytes,
        virtual_size_mib,
        &dataset,
        &zvol_rdsk,
        &opts.output_dir,
        opts.vendor,
    )
    .await;

    eprintln!("Destroying zvol: {}", dataset);
    let _ = zfs::destroy_recursive(&dataset).await;

    result
}

#[allow(clippy::too_many_arguments)] // local helper
async fn build_image(
    resolved: &ResolvedImage,
    src_path: &Path,
    source_sha256: &str,
    virtual_size_bytes: u64,
    virtual_size_mib: u64,
    dataset: &str,
    zvol_rdsk: &Path,
    output_dir: &Path,
    vendor: &str,
) -> Result<PipelineOutputs> {
    zfs::create_zvol(dataset, virtual_size_mib).await?;

    eprintln!(
        "Writing image to zvol ({} bytes from {}) ...",
        virtual_size_bytes,
        match resolved.format {
            SourceFormat::Qcow2 => "qcow2",
            SourceFormat::Raw => "raw",
            SourceFormat::Xz => "xz",
        }
    );
    write_to_zvol(src_path, resolved.format, zvol_rdsk, virtual_size_bytes).await?;

    let snap = format!("{dataset}@image");
    eprintln!("Snapshotting zvol ...");
    zfs::snap(&snap).await?;

    let stub = format!("{}-{}-{}", vendor, resolved.series, resolved.version);
    let zfs_path = output_dir.join(format!("{stub}.x86_64.zfs"));
    let gz_path = output_dir.join(format!("{stub}.x86_64.zfs.gz"));
    let manifest_path = output_dir.join(format!("{stub}.json"));

    eprintln!("Exporting ZFS stream → {} ...", zfs_path.display());
    zfs::send_to_file(&snap, &zfs_path).await?;

    eprintln!("Compressing image ...");
    let status = tokio::process::Command::new("gzip")
        .arg("-f")
        .arg(&zfs_path)
        .status()
        .await
        .context("spawn gzip")?;
    if !status.success() {
        bail!("gzip exited {status}");
    }

    let sha1 = sha1_file(&gz_path).await?;
    let size = tokio::fs::metadata(&gz_path).await?.len();

    let manifest_uuid = stable_manifest_uuid(source_sha256);
    let published_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let inputs = ManifestInputs {
        uuid: manifest_uuid,
        name: format!("{}-{}-nocloud", vendor, resolved.series),
        version: resolved.version.clone(),
        published_at,
        os: resolved.os.clone(),
        sha1,
        size,
        description: resolved.description.clone(),
        homepage: resolved.homepage.to_string(),
        ssh_key: resolved.ssh_key,
        image_size_mib: virtual_size_mib,
    };
    let body = serde_json::to_vec_pretty(&manifest::build(&inputs))?;
    tokio::fs::write(&manifest_path, body).await?;

    Ok(PipelineOutputs {
        gz_path,
        manifest_path,
        manifest_uuid,
    })
}

/// Read the virtual disk size from the source. For qcow2 we parse the
/// header; for raw we use the file size; xz is deferred.
async fn read_virtual_size(path: &Path, format: SourceFormat) -> Result<u64> {
    let path = path.to_path_buf();
    match format {
        SourceFormat::Raw => Ok(tokio::fs::metadata(&path).await?.len()),
        SourceFormat::Qcow2 => tokio::task::spawn_blocking(move || -> Result<u64> {
            let dyn_qcow = qcow::open(&path)
                .map_err(|e| anyhow::anyhow!("open qcow2 {}: {e}", path.display()))?;
            let qcow2 = dyn_qcow.unwrap_qcow2();
            Ok(qcow2.header.size)
        })
        .await
        .context("qcow2 header read task panicked")?,
        SourceFormat::Xz => bail!("xz source format not yet implemented"),
    }
}

async fn write_to_zvol(
    src_path: &Path,
    format: SourceFormat,
    zvol_rdsk: &Path,
    virtual_size: u64,
) -> Result<()> {
    let pb = ProgressBar::new(virtual_size);
    pb.set_style(byte_progress_style("Writing"));

    let src_path = src_path.to_path_buf();
    let zvol_rdsk = zvol_rdsk.to_path_buf();
    let pb_clone = pb.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<()> {
        match format {
            SourceFormat::Qcow2 => {
                // Two file handles on the same path: one for the
                // metadata parse (consumed by `qcow::open`), one passed
                // into the cluster reader. The qcow crate's docs note
                // the reader file must be the same source as the
                // header parse, but two reads of the same on-disk file
                // satisfy that.
                let dyn_qcow = qcow::open(&src_path)
                    .map_err(|e| anyhow::anyhow!("open qcow2: {e}"))?;
                let qcow2 = dyn_qcow.unwrap_qcow2();
                let mut file = std::fs::File::open(&src_path)
                    .with_context(|| format!("reopen {}", src_path.display()))?;
                let mut reader = qcow2.reader(&mut file);
                copy_with_progress(&mut reader, &zvol_rdsk, virtual_size, &pb_clone)
            }
            SourceFormat::Raw => {
                let mut reader = std::fs::File::open(&src_path)
                    .with_context(|| format!("open {}", src_path.display()))?;
                copy_with_progress(&mut reader, &zvol_rdsk, virtual_size, &pb_clone)
            }
            SourceFormat::Xz => bail!("xz source format not yet implemented"),
        }
    })
    .await
    .context("zvol write task panicked")?;

    pb.finish_and_clear();
    result
}

fn copy_with_progress(
    reader: &mut dyn Read,
    zvol_rdsk: &Path,
    total: u64,
    pb: &ProgressBar,
) -> Result<()> {
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .open(zvol_rdsk)
        .with_context(|| format!("open zvol {} for write", zvol_rdsk.display()))?;
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    let mut remaining = total;
    while remaining > 0 {
        let to_read = std::cmp::min(buf.len() as u64, remaining) as usize;
        reader
            .read_exact(&mut buf[..to_read])
            .with_context(|| format!("read source ({remaining} bytes remaining)"))?;
        writer
            .write_all(&buf[..to_read])
            .with_context(|| format!("write zvol ({} bytes written)", total - remaining))?;
        remaining -= to_read as u64;
        pb.inc(to_read as u64);
    }
    writer.flush().context("flush zvol")?;
    Ok(())
}

async fn download_with_progress(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<()> {
    let resp = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("status from {url}"))?;

    let total = resp.content_length().unwrap_or(0);
    let pb = if total > 0 {
        let pb = ProgressBar::new(total);
        pb.set_style(byte_progress_style("Downloading"));
        pb
    } else {
        // Server didn't send Content-Length (rare for static cloud
        // images, but possible on a redirect chain or chunked
        // transfer). Fall back to a spinner.
        let pb = ProgressBar::new_spinner();
        pb.set_style(spinner_progress_style("Downloading"));
        pb
    };

    let mut f = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("create {}", dest.display()))?;
    let mut stream = resp;
    while let Some(chunk) = stream.chunk().await? {
        f.write_all(&chunk).await?;
        pb.inc(chunk.len() as u64);
    }
    f.flush().await?;
    pb.finish_and_clear();
    Ok(())
}

fn byte_progress_style(prefix: &str) -> ProgressStyle {
    ProgressStyle::with_template(&format!(
        "{prefix} [{{elapsed_precise}}] {{bar:40.cyan/blue}} \
         {{bytes:>10}}/{{total_bytes:<10}} ({{bytes_per_sec}}, ETA {{eta}})"
    ))
    .unwrap_or_else(|_| ProgressStyle::default_bar())
}

fn spinner_progress_style(prefix: &str) -> ProgressStyle {
    ProgressStyle::with_template(&format!(
        "{prefix} [{{elapsed_precise}}] {{spinner}} {{bytes}} ({{bytes_per_sec}})"
    ))
    .unwrap_or_else(|_| ProgressStyle::default_spinner())
}

/// Compute SHA-1 by shelling out to illumos `digest -a sha1`. The image
/// manifest format mandates SHA-1 for the file digest (legacy IMGAPI
/// requirement). Adding a SHA-1 crate just for this is more weight
/// than calling the system tool.
async fn sha1_file(file: &Path) -> Result<String> {
    let out = tokio::process::Command::new("digest")
        .args(["-a", "sha1"])
        .arg(file)
        .stdout(Stdio::piped())
        .output()
        .await
        .context("spawn digest")?;
    if !out.status.success() {
        bail!(
            "digest -a sha1 exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8(out.stdout)
        .context("digest stdout was not UTF-8")?
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_uuid_is_stable_per_sha256() {
        // Same input → same UUID every call.
        let a = stable_manifest_uuid("5c3ddb00f60bc455dac0862fabe9d8bacec46c33ac1751143c5c3683404b110d");
        let b = stable_manifest_uuid("5c3ddb00f60bc455dac0862fabe9d8bacec46c33ac1751143c5c3683404b110d");
        assert_eq!(a, b);
    }

    #[test]
    fn manifest_uuid_differs_for_different_sha256() {
        let a = stable_manifest_uuid("5c3ddb00f60bc455dac0862fabe9d8bacec46c33ac1751143c5c3683404b110d");
        let b = stable_manifest_uuid("6e7016f2c9f4d3c00f48789eb6b9043ba2172ccc1b6b1eaf3ed1e29dd3e52bb3");
        assert_ne!(a, b);
    }

    #[test]
    fn manifest_uuid_is_v5() {
        let u = stable_manifest_uuid("aa");
        assert_eq!(u.get_version_num(), 5);
    }
}
