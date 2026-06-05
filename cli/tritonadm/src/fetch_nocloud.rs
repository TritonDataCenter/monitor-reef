// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonadm image-v1 fetch-nocloud` — drive [`nocloud_import`] against
//! a chosen vendor and, optionally, ship the result to a vnext tritond
//! IMGAPI surface via an in-cluster mantad.
//!
//! Two targets:
//!
//! - `--target file` (default): runs the pipeline and leaves
//!   `<vendor>-<series>-<stamp>.zfs.gz` + `.json` in `--output-dir`.
//!   Equivalent to the upstream tritonadm File target. Works for
//!   anyone who wants to inspect the artifact before shipping.
//!
//! - `--target tritond`: in addition, uploads the gz to mantad (an
//!   S3-compatible blob store, conventionally the in-cluster
//!   `triton-mantad-0` zone), then POSTs the IMGAPI manifest +
//!   manta_url + sha256 to the configured tritond endpoint at
//!   `POST /v1/silos/{silo_id}/imgapi-images`. Requires `--silo`
//!   and the `MANTAD_*` env vars listed below.
//!
//! ## Environment for `--target tritond`
//!
//! | Var | Purpose |
//! |---|---|
//! | `MANTAD_ENDPOINT` | e.g. `http://172.16.96.6:7443` |
//! | `MANTAD_REGION` | SigV4 region (default `us-east-1`) |
//! | `MANTAD_BUCKET` | default `triton-images` |
//! | `MANTAD_ACCESS_KEY_ID` | SigV4 access key (mantad root) |
//! | `MANTAD_SECRET_ACCESS_KEY` | SigV4 secret key (mantad root) |

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;
use sha1::Digest as _;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Target {
    /// Leave the artifacts in `--output-dir`; print the
    /// `imgadm install` command that would import them.
    File,
    /// File + S3-PUT the gz to mantad + POST the manifest to tritond.
    Tritond,
}

pub struct Opts {
    pub vendor: nocloud_import::vendor::Vendor,
    pub release: String,
    pub target: Target,
    pub silo: Option<Uuid>,
    pub output_dir: Option<PathBuf>,
    pub workdir: Option<PathBuf>,
    pub zfs_dataset: Option<String>,
    pub dry_run: bool,
    pub insecure_no_verify: bool,
    /// Endpoint of the tritond instance to register the image with.
    /// Only consulted when `target == Tritond`. Falls through to
    /// the global `--endpoint` / `TRITONADM_ENDPOINT` chain.
    pub tritond_endpoint: Option<String>,
    /// Bearer token for the tritond endpoint (operator JWT or API
    /// key value). Only consulted when `target == Tritond`.
    pub tritond_bearer: Option<String>,
}

pub async fn run(opts: Opts) -> Result<()> {
    // Async reqwest client with bundled Mozilla webpki roots —
    // see crate::http::async_client for why the default rustls
    // config won't load CAs on illumos.
    let http = crate::http::async_client().context("build async reqwest client")?;

    let profile = nocloud_import::vendor::lookup(opts.vendor);
    let resolved = profile
        .resolve(&opts.release, &http)
        .await
        .with_context(|| format!("resolve {:?} @ {}", opts.vendor, opts.release))?;
    println!(
        "resolved: {} {} -> {}",
        resolved.series, resolved.version, resolved.url
    );
    if let Some(ref sha) = resolved.expected_sha256 {
        println!("expected sha256: {sha}");
    }
    if opts.dry_run {
        println!("dry-run; not building");
        return Ok(());
    }

    // Extract fields needed for the post-pipeline registration before
    // we consume `opts` by the unwrap_or_else calls below.
    let target = opts.target;
    let silo = opts.silo;
    let tritond_endpoint = opts.tritond_endpoint;
    let tritond_bearer = opts.tritond_bearer;

    // Default the workdir + output_dir under /var/tmp/tritonadm-nocloud/
    // so the user only needs to override when they care.
    let base = PathBuf::from("/var/tmp/tritonadm-nocloud");
    let workdir = opts.workdir.unwrap_or_else(|| base.join("work"));
    let output_dir = opts.output_dir.unwrap_or_else(|| base.join("out"));
    let zfs_dataset = opts.zfs_dataset.unwrap_or_else(|| "zones".to_string());

    let vendor_label = nocloud_import::enum_to_display(&opts.vendor);
    let pipeline_opts = nocloud_import::pipeline::PipelineOptions {
        vendor: &vendor_label,
        workdir,
        output_dir,
        zfs_dataset,
        http: &http,
        insecure_no_verify: opts.insecure_no_verify,
    };

    let outputs = nocloud_import::pipeline::run(resolved, pipeline_opts)
        .await
        .context("nocloud-import pipeline")?;
    println!("manifest: {}", outputs.manifest_path.display());
    println!("gz:       {}", outputs.gz_path.display());
    println!("uuid:     {}", outputs.manifest_uuid);

    if matches!(target, Target::File) {
        println!(
            "next: imgadm install -m {} -f {}",
            outputs.manifest_path.display(),
            outputs.gz_path.display()
        );
        return Ok(());
    }

    // target == Tritond
    let silo = silo.ok_or_else(|| anyhow!("--target tritond requires --silo <UUID>"))?;
    register_with_tritond(silo, tritond_endpoint, tritond_bearer, &outputs).await
}

async fn register_with_tritond(
    silo: Uuid,
    tritond_endpoint: Option<String>,
    tritond_bearer: Option<String>,
    outputs: &nocloud_import::pipeline::PipelineOutputs,
) -> Result<()> {
    let endpoint = tritond_endpoint
        .ok_or_else(|| anyhow!("--target tritond needs --endpoint or TRITONADM_ENDPOINT"))?;
    let bearer = tritond_bearer
        .ok_or_else(|| anyhow!("--target tritond needs an --api-key or login session"))?;

    // mantad config from env. Fail fast and clearly if anything is
    // missing — the operator's typo here causes a five-minute upload
    // failure instead of a one-second skip.
    let mantad_endpoint = std::env::var("MANTAD_ENDPOINT")
        .context("MANTAD_ENDPOINT env (e.g. http://172.16.96.6:7443)")?;
    let mantad_region = std::env::var("MANTAD_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let mantad_bucket =
        std::env::var("MANTAD_BUCKET").unwrap_or_else(|_| "triton-images".to_string());
    let mantad_ak = std::env::var("MANTAD_ACCESS_KEY_ID").context("MANTAD_ACCESS_KEY_ID env")?;
    let mantad_sk =
        std::env::var("MANTAD_SECRET_ACCESS_KEY").context("MANTAD_SECRET_ACCESS_KEY env")?;

    let store = imgapi_blob_manta::BlobStore::new(
        url::Url::parse(&mantad_endpoint).context("MANTAD_ENDPOINT must be a URL")?,
        mantad_region,
        mantad_bucket,
        mantad_ak,
        mantad_sk,
    )
    .context("BlobStore::new")?;

    println!(
        "uploading to mantad bucket: {}",
        store.url_for(outputs.manifest_uuid)
    );
    let upload = store
        .upload(outputs.manifest_uuid, &outputs.gz_path)
        .await
        .context("BlobStore::upload")?;
    println!(
        "uploaded: sha1={} size={} url={}",
        upload.sha1, upload.size, upload.public_url
    );

    // Re-hash the gz with SHA-256 for the tritond IMGAPI body's
    // defense-in-depth check. The pipeline already computed it
    // internally but doesn't surface it; re-hashing a ~50 MB file
    // takes a second or two and avoids modifying the lib's public
    // surface for this single caller.
    let sha256 = sha256_file(&outputs.gz_path).await?;

    // Read the manifest the pipeline produced + parse it as
    // imgapi_manifest::Manifest so we POST a typed body that
    // matches tritond's NewImageFromImgapi schema.
    let manifest_bytes = tokio::fs::read(&outputs.manifest_path)
        .await
        .with_context(|| format!("read {}", outputs.manifest_path.display()))?;
    let manifest = imgapi_manifest::Manifest::parse(&manifest_bytes)
        .context("re-parse pipeline's manifest as imgapi_manifest::Manifest")?;

    let body = serde_json::json!({
        "manifest": manifest,
        "manta_url": upload.public_url,
        "sha256": sha256,
    });

    let url = format!(
        "{}/v1/silos/{silo}/imgapi-images",
        endpoint.trim_end_matches('/')
    );
    println!("POST {url}");
    let client =
        crate::http::async_client().context("build async reqwest client for tritond POST")?;
    let resp = client
        .post(&url)
        .bearer_auth(&bearer)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("tritond POST returned {status}: {text}"));
    }
    println!("registered: {text}");
    Ok(())
}

async fn sha256_file(path: &std::path::Path) -> Result<String> {
    use tokio::io::AsyncReadExt as _;
    let mut f = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
