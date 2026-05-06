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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use vendor::Vendor;

/// What to do with the produced `*.zfs.gz` + `*.json` pair.
#[derive(clap::ValueEnum, serde::Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    /// Leave the artifacts in `--output-dir` and print the
    /// `imgadm install` invocation that would import them.
    #[default]
    File,
    /// Run `imgadm install -m <manifest> -f <gz>` against the
    /// local SmartOS image store. GZ-only.
    Smartos,
    /// Push the manifest+file to IMGAPI via the existing
    /// `tritonadm image import` machinery.
    Imgapi,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::enum_to_display(self))
    }
}

pub struct FetchOpts {
    /// Built-in vendor profile. Set when invoked as
    /// `--vendor X --release Y`; `None` when `vendor_toml` is set.
    pub vendor: Option<Vendor>,
    /// Vendor-specific release token. Required with `vendor`; unused
    /// with `vendor_toml`.
    pub release: Option<String>,
    /// External TOML profile path. Set when invoked as
    /// `--vendor-toml PATH`; mutually exclusive with `vendor`.
    pub vendor_toml: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub workdir: Option<PathBuf>,
    pub insecure_no_verify: bool,
    pub expected_sha256: Option<String>,
    pub dataset: Option<String>,
    pub dry_run: bool,
    pub target: Target,
    /// Lazy IMGAPI URL, only consumed for `Target::Imgapi`. Passed
    /// through as a `Result` so File / Smartos targets don't fail
    /// when no headnode is reachable from the builder zone.
    pub imgapi_url: Result<String>,
    pub updates_url: Option<String>,
}

pub async fn run(mut opts: FetchOpts) -> Result<()> {
    // Auto-promote to --dry-run on non-SmartOS hosts so a developer
    // can smoke-test vendor metadata fetching from a Mac/Linux box
    // without remembering the flag. The build itself still requires
    // SmartOS; this just lets the resolve-and-print path succeed.
    let on_smartos = is_smartos()?;
    if !on_smartos && !opts.dry_run {
        eprintln!(
            "note: not running on SmartOS; forcing --dry-run \
             (real builds require zfs(8) and a delegated dataset)"
        );
        opts.dry_run = true;
    }

    // Vendor resolution is just HTTP, so it runs anywhere — doing it
    // before the SmartOS-specific preflights lets `--dry-run` exercise
    // release resolution + verifier wiring on a dev box.
    let http = triton_tls::build_http_client(false)
        .await
        .map_err(|e| anyhow::anyhow!("build http client: {e}"))?;

    let (vendor_label, mut resolved) = match (&opts.vendor_toml, opts.vendor, &opts.release) {
        (Some(path), None, _) => vendor::custom_toml::load(path)
            .await
            .with_context(|| format!("load TOML profile {}", path.display()))?,
        (None, Some(vendor), Some(release)) => {
            let profile = vendor::lookup(vendor);
            let resolved = profile
                .resolve(release, &http)
                .await
                .with_context(|| format!("resolve {vendor}/{release}"))?;
            (vendor.to_string(), resolved)
        }
        // clap's `required_unless_present` / `conflicts_with_all`
        // already enforces the valid combinations, so anything else
        // means the dispatcher in image.rs got out of sync with this
        // module — a programmer bug, not user input.
        _ => anyhow::bail!("internal: invalid (--vendor, --release, --vendor-toml) combination"),
    };

    // `--expected-sha256 <hex>` overrides whatever verifier the vendor
    // chose with a pinned-hash check. Useful for vendors that don't
    // publish per-image hashes (Talos), and for one-off pinning when
    // the operator has obtained a hash out-of-band.
    if let Some(ref hex) = opts.expected_sha256 {
        let hex = hex.trim().to_lowercase();
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!(
                "--expected-sha256 must be 64 lowercase hex chars, got {:?}",
                opts.expected_sha256.as_deref().unwrap_or("")
            );
        }
        resolved.verifier = Box::new(verify::Sha256Pinned(hex.clone()));
        resolved.expected_sha256 = Some(hex);
    }

    let stub = format!("{}-{}", vendor_label, resolved.series);
    let workdir = opts
        .workdir
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/tritonadm/nocloud/cache/{stub}")));
    let output_dir = opts
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/tritonadm/nocloud/image/{stub}")));

    if opts.dry_run {
        // Best-effort dataset display: use --dataset if given, else try
        // default_dataset() (works on SmartOS), else a placeholder so
        // the plan still renders on a dev box.
        let dataset = opts
            .dataset
            .clone()
            .or_else(|| default_dataset().ok())
            .unwrap_or_else(|| "(default: zones/<zone>/data, resolved at runtime)".to_string());
        print_plan(
            &opts,
            &vendor_label,
            &resolved,
            &dataset,
            &workdir,
            &output_dir,
        );
        return Ok(());
    }

    // From here on we are committing to a real build, which requires
    // SmartOS, a usable target, and a delegated dataset.
    let zone = current_zone()?;
    if opts.target == Target::Smartos && zone != "global" {
        anyhow::bail!(
            "--target smartos requires running in the SmartOS GZ \
             (zonename={zone}); produce files with --target file and \
             run `imgadm install` from the GZ instead"
        );
    }
    if opts.target == Target::Imgapi
        && let Err(e) = opts.imgapi_url.as_ref()
    {
        anyhow::bail!("--target imgapi requires a working IMGAPI URL: {e}");
    }

    let dataset = match opts.dataset.clone() {
        Some(d) => d,
        None => default_dataset()?,
    };

    // Serialize concurrent builds of the same (vendor, series). Lock
    // is keyed on the workdir, so different vendor/release pairs run
    // in parallel without contention. flock state lives on the FD;
    // the kernel releases it on any process exit, so a crashed run
    // won't leave a stuck lock behind. The empty .lock file on disk
    // is harmless.
    tokio::fs::create_dir_all(&workdir)
        .await
        .with_context(|| format!("create {}", workdir.display()))?;
    let _lock_guard = acquire_workdir_lock(&workdir)
        .with_context(|| format!("acquire workdir lock for {}", workdir.display()))?;

    let outputs = pipeline::run(
        resolved,
        pipeline::PipelineOptions {
            vendor: &vendor_label,
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

    match opts.target {
        Target::File => {
            println!("To install on this SmartOS host:");
            println!(
                "  imgadm install -f {} -m {}",
                outputs.gz_path.display(),
                outputs.manifest_path.display()
            );
        }
        Target::Smartos => {
            install_via_imgadm(&outputs.gz_path, &outputs.manifest_path).await?;
            println!("Installed image {} via imgadm.", outputs.manifest_uuid);
        }
        Target::Imgapi => {
            push_to_imgapi(&opts, &outputs).await?;
        }
    }
    Ok(())
}

/// Shell out to `imgadm install -m <manifest> -f <gz>`. The flags are
/// passed in `-m`/`-f` order to mirror what the operator sees when
/// `--target file` prints the suggested invocation. GZ-only; the
/// caller must have rejected NGZs already.
async fn install_via_imgadm(gz: &Path, manifest: &Path) -> Result<()> {
    println!("Installing into the local SmartOS image store via imgadm...");
    let status = tokio::process::Command::new("imgadm")
        .arg("install")
        .arg("-m")
        .arg(manifest)
        .arg("-f")
        .arg(gz)
        .status()
        .await
        .context("spawn imgadm install")?;
    if !status.success() {
        anyhow::bail!("imgadm install exited {status}");
    }
    Ok(())
}

/// Push the produced manifest+file to IMGAPI by reusing the
/// `tritonadm image import` code path. Compression is left as
/// `None` so the helper picks `gzip` from the manifest's
/// `files[0].compression` (always set by our pipeline).
async fn push_to_imgapi(opts: &FetchOpts, outputs: &pipeline::PipelineOutputs) -> Result<()> {
    let imgapi_url = opts
        .imgapi_url
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .clone();
    let http = triton_tls::build_http_client(false)
        .await
        .map_err(|e| anyhow::anyhow!("build http client: {e}"))?;
    let client = imgapi_client::Client::new_with_client(&imgapi_url, http.clone());
    let typed_client = imgapi_client::TypedClient::new_with_client(&imgapi_url, http);

    let manifest = outputs.manifest_path.to_string_lossy().into_owned();
    let file = outputs.gz_path.to_string_lossy().into_owned();
    super::import_manifest_and_file(
        &client,
        &typed_client,
        &manifest,
        &file,
        None,
        opts.updates_url.as_deref(),
    )
    .await
}

fn print_plan(
    opts: &FetchOpts,
    vendor_label: &str,
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
    let stub = format!("{}-{}-{}", vendor_label, resolved.series, resolved.version);

    println!("Resolved upstream image:");
    println!("  Target:        {}", opts.target);
    println!("  Vendor:        {}", vendor_label);
    println!("  Codename:      {}", resolved.series);
    println!("  Version:       {}", resolved.version);
    println!("  URL:           {}", resolved.url);
    println!(
        "  Format:        {}",
        match resolved.format {
            vendor::SourceFormat::Qcow2 => "qcow2",
            vendor::SourceFormat::Raw => "raw",
            vendor::SourceFormat::Xz => "xz",
            vendor::SourceFormat::Vmdk => "vmdk",
            vendor::SourceFormat::RawGz => "raw.gz",
        }
    );
    match &resolved.expected_sha256 {
        Some(s) => {
            println!("  SHA-256:       {s}");
            let manifest_uuid = pipeline::stable_manifest_uuid(s);
            println!("  Manifest UUID: {manifest_uuid}  (derived from sha256)");
        }
        None => {
            println!("  SHA-256:       (computed locally after download)");
            println!("  Manifest UUID: (derived from local sha256 after download)");
        }
    }

    println!();
    if resolved.url.scheme() == "file" {
        // file:// sources are read in place — no workdir cache.
        println!("Source file (read in place):");
        match resolved.url.to_file_path() {
            Ok(p) => println!("  {}", p.display()),
            Err(()) => println!("  {} (not a usable file:// path)", resolved.url),
        }
        println!();
    }
    println!("Would write to:");
    if resolved.url.scheme() != "file" {
        println!("  Cache file:    {}", workdir.join(src_filename).display());
    }
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
    println!(
        "  name:          {}-{}-nocloud",
        vendor_label, resolved.series
    );
    println!("  version:       {}", resolved.version);
    println!("  os:            {}", resolved.os);
    println!("  ssh_key req'd: {}", resolved.ssh_key);

    println!();
    println!("(--dry-run: nothing was downloaded, written, or created.)");
}

/// Acquire an exclusive `flock` on `<workdir>/.lock`, fail-fast if
/// another process holds it. The returned `File` must outlive the
/// pipeline; closing it (drop) releases the lock. The kernel also
/// releases the lock on any process exit, so a SIGKILL'd run won't
/// leave a stuck lock on disk. The empty `.lock` file itself is
/// harmless and stays around between runs.
fn acquire_workdir_lock(workdir: &Path) -> Result<std::fs::File> {
    let lock_path = workdir.join(".lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    // `std::fs::File::try_lock` (stable since Rust 1.89) wraps
    // `flock(LOCK_EX | LOCK_NB)` with no `unsafe` in our code.
    match lock_file.try_lock() {
        Ok(()) => Ok(lock_file),
        Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
            "another tritonadm fetch-nocloud build is already running for this \
             (vendor, release); wait for it to finish, or pass a different \
             --workdir to run concurrently"
        ),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("flock failed"),
    }
}

/// Detect whether we're running on SmartOS. `uname -v` on illumos
/// distros starts with `joyent_…`. Any other prefix (Darwin, Linux,
/// FreeBSD, …) means a dev box where dry-run is the only sensible
/// thing to do.
fn is_smartos() -> Result<bool> {
    let v = std::process::Command::new("uname")
        .arg("-v")
        .output()
        .context("spawn uname -v")?;
    Ok(String::from_utf8_lossy(&v.stdout).starts_with("joyent_"))
}

/// Run `zonename` and return its trimmed output (`global` for the GZ,
/// the zone name for NGZs).
fn current_zone() -> Result<String> {
    let zone = std::process::Command::new("zonename")
        .output()
        .context("spawn zonename")?;
    if !zone.status.success() {
        anyhow::bail!("zonename exited {}", zone.status);
    }
    Ok(String::from_utf8_lossy(&zone.stdout).trim().to_string())
}

/// Default dataset for the temporary build zvol.
///
/// In an NGZ this is the delegated dataset (`zones/<zone>/data` with
/// `zoned=on`); in the GZ we drop directly under `zones`.
fn default_dataset() -> Result<String> {
    let zone = current_zone()?;
    if zone == "global" {
        return Ok("zones".to_string());
    }
    let dataset = format!("zones/{zone}/data");
    let zoned = std::process::Command::new("zfs")
        .args(["get", "-H", "-o", "value", "zoned", &dataset])
        .output()
        .context("spawn zfs get zoned")?;
    if !zoned.status.success() || String::from_utf8_lossy(&zoned.stdout).trim() != "on" {
        anyhow::bail!(
            "delegated dataset {dataset} not available or not zoned. \
             Pass --dataset to override."
        );
    }
    Ok(dataset)
}
