// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritoncloud-publish` — push a built release artifact to Manta and
//! update the signed channel manifest.
//!
//! See `rfd/00006/01-pipeline-and-channels.md` for the broader design.
//!
//! ## Subcommands
//!
//! - `init-channel`: create an empty `<channel>.json` (signed).
//! - `image`: publish a zone image (manifest + content.zfs.gz).
//! - `agent`: publish a per-CN GZ tarball (`tritonagent`, `proteusadm`).
//! - `tcadm`: publish a tcadm binary for a target triple.
//! - `show`: dump the current channel manifest to stdout.
//!
//! ## Environment
//!
//! - `MANTA_URL`, `MANTA_USER`, `MANTA_KEY_ID`: standard node-manta.
//! - `TRITONCLOUD_MANTA_BASE`: defaults to
//!   `/nick.wilkens@mnxsolutions.com/public/tritoncloud`.
//! - `TRITONCLOUD_HTTPS_BASE`: defaults to
//!   `https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud`.
//! - `TRITONCLOUD_PUBLISHER`: defaults to `$MANTA_USER` if set, else
//!   `unknown`. Goes into the manifest's `publisher` field.
//! - `TRITONCLOUD_SECRET_KEY`: path to the minisign `.key`. Defaults
//!   to `~/.config/tritoncloud/publisher.key`.
//! - `MINISIGN_PASSWORD`: if set, used by `minisign -S` instead of
//!   prompting on stdin.

mod channel;
mod manta;
mod signing;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use sha2::{Digest, Sha256};
use tracing::info;
use tracing_subscriber::EnvFilter;
use triton_channel::{AgentEntry, ChannelManifest, ImageEntry, ServiceEntry, TcadmEntry};

use crate::channel::{ChannelLocator, fetch_or_init, new_empty, publish};
use crate::manta::mput;

/// Default Manta directory under which all tritoncloud artifacts live.
const DEFAULT_MANTA_BASE: &str = "/nick.wilkens@mnxsolutions.com/public/tritoncloud";

/// Default public HTTPS prefix corresponding to `DEFAULT_MANTA_BASE`.
const DEFAULT_HTTPS_BASE: &str =
    "https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud";

#[derive(Debug, Parser)]
#[command(name = "tritoncloud-publish", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Channel name. Required for every subcommand.
    #[arg(long, global = true, default_value = "edge")]
    channel: String,

    /// Manta directory base. Defaults to `TRITONCLOUD_MANTA_BASE` or
    /// the project default.
    #[arg(long, global = true, env = "TRITONCLOUD_MANTA_BASE", default_value = DEFAULT_MANTA_BASE)]
    manta_base: String,

    /// Public HTTPS prefix corresponding to `--manta-base`.
    #[arg(long, global = true, env = "TRITONCLOUD_HTTPS_BASE", default_value = DEFAULT_HTTPS_BASE)]
    https_base: String,

    /// Operator identifier recorded in the manifest's `publisher`
    /// field. Defaults to `$TRITONCLOUD_PUBLISHER` or `$MANTA_USER`.
    #[arg(long, global = true, env = "TRITONCLOUD_PUBLISHER")]
    publisher: Option<String>,

    /// Path to the publisher's minisign secret key.
    #[arg(
        long,
        global = true,
        env = "TRITONCLOUD_SECRET_KEY",
        default_value = "~/.config/tritoncloud/publisher.key"
    )]
    secret_key: PathBuf,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a fresh empty channel manifest. Refuses if one already
    /// exists at the channel path.
    InitChannel,

    /// Dump the current channel manifest to stdout.
    Show,

    /// Publish a zone image (imgadm manifest + content.zfs.gz).
    Image(ImageArgs),

    /// Publish a per-CN GZ tarball (tritonagent, proteusadm).
    Agent(AgentArgs),

    /// Publish a zone-resident service binary (tritond, admin-backend) as
    /// a binary-swap update target (`services.<name>` in the manifest).
    Service(ServiceArgs),

    /// Publish a tcadm binary for one target triple.
    Tcadm(TcadmArgs),

    /// Publish the bootstrap install.sh script + its detached
    /// signature to `~~/public/tritoncloud/install.sh`. The script
    /// itself is not channel-scoped (the embedded pubkey + default
    /// channel URL are baked into the script source); operators
    /// curl it directly. This subcommand only re-signs and uploads.
    InstallSh {
        /// Local path to the install.sh source.
        #[arg(long, default_value = "tools/install.sh")]
        source: PathBuf,
    },
}

#[derive(Debug, Args)]
struct ImageArgs {
    /// Canonical image name (key into `images` in the manifest), e.g.
    /// `triton-tritond`.
    #[arg(long)]
    name: String,

    /// Build stamp (`YYYYMMDDTHHMMSSZ`). Becomes the basename of the
    /// uploaded files in `images/<name>/`.
    #[arg(long)]
    stamp: String,

    /// Image UUID embedded in the imgadm manifest. Surfaced in the
    /// channel manifest so consumers can recognize an
    /// already-installed image without downloading.
    #[arg(long)]
    uuid: uuid::Uuid,

    /// Local path to the imgadm manifest JSON.
    #[arg(long)]
    manifest: PathBuf,

    /// Local path to the imgadm content blob (`<uuid>.zfs.gz`).
    #[arg(long)]
    content: PathBuf,

    /// Oldest PI buildstamp this image is known to coexist with.
    #[arg(long)]
    pi_min: Option<String>,

    /// On-disk data format version this image writes.
    #[arg(long)]
    data_format_version: u32,

    /// Oldest on-disk data format this image can attach to.
    #[arg(long)]
    data_format_min_read: u32,
}

#[derive(Debug, Args)]
struct ServiceArgs {
    /// Canonical service name (key into `services`), e.g. `tritond`,
    /// `admin-backend`. This is what `tcadm update <name>` resolves.
    #[arg(long)]
    name: String,

    /// Build stamp.
    #[arg(long)]
    stamp: String,

    /// Local path to the service binary.
    #[arg(long)]
    binary: PathBuf,

    /// Alias of the zone the binary lives in (e.g. `triton-tritond`).
    #[arg(long)]
    zone: String,

    /// Absolute path of the binary INSIDE that zone
    /// (e.g. `/opt/triton/tritond/bin/tritond`).
    #[arg(long)]
    bin_path: String,

    /// SMF service to restart after the swap (e.g. `site/triton-tritond`).
    #[arg(long)]
    smf: String,

    /// Oldest PI buildstamp this binary is known to coexist with.
    #[arg(long)]
    pi_min: Option<String>,
}

#[derive(Debug, Args)]
struct AgentArgs {
    /// Canonical agent name, e.g. `tritonagent`, `proteusadm`.
    #[arg(long)]
    name: String,

    /// Build stamp.
    #[arg(long)]
    stamp: String,

    /// Local path to the agent tarball.
    #[arg(long)]
    tarball: PathBuf,

    /// Oldest PI buildstamp this agent is known to coexist with.
    #[arg(long)]
    pi_min: Option<String>,
}

#[derive(Debug, Args)]
struct TcadmArgs {
    /// Build stamp.
    #[arg(long)]
    stamp: String,

    /// Rust target triple, e.g. `x86_64-unknown-illumos`.
    #[arg(long)]
    target: String,

    /// Local path to the tcadm tarball.
    #[arg(long)]
    tarball: PathBuf,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    let publisher = cli
        .publisher
        .clone()
        .or_else(|| std::env::var("MANTA_USER").ok())
        .unwrap_or_else(|| "unknown".to_string());

    let locator = ChannelLocator {
        channel: cli.channel.clone(),
        manta_base: cli.manta_base.clone(),
        https_base: cli.https_base.clone(),
        publisher,
    };

    let secret_key = expand_tilde(&cli.secret_key)?;

    match cli.command {
        Command::InitChannel => do_init_channel(&locator, &secret_key),
        Command::Show => do_show(&locator),
        Command::Image(args) => do_image(&locator, &secret_key, args),
        Command::Agent(args) => do_agent(&locator, &secret_key, args),
        Command::Service(args) => do_service(&locator, &secret_key, args),
        Command::Tcadm(args) => do_tcadm(&locator, &secret_key, args),
        Command::InstallSh { source } => do_install_sh(&locator, &secret_key, source),
    }
}

fn do_install_sh(locator: &ChannelLocator, secret_key: &Path, source: PathBuf) -> Result<()> {
    // install.sh lives at the top of `~~/public/tritoncloud/` (not
    // channel-scoped) so it has a stable curl URL. Sign in-process so
    // MINISIGN_PASSWORD is honored, then mput both files.
    let workdir = tempfile::tempdir().context("tempdir")?;
    let local = workdir.path().join("install.sh");
    let sig = workdir.path().join("install.sh.minisig");

    fs::copy(&source, &local)
        .with_context(|| format!("copy {} -> {}", source.display(), local.display()))?;
    crate::signing::sign_file(secret_key, &local, &sig)?;

    let remote = format!("{}/install.sh", locator.manta_base);
    let remote_sig = format!("{remote}.minisig");
    mput(&local, &remote)?;
    mput(&sig, &remote_sig)?;
    info!("install.sh published");
    Ok(())
}

fn do_init_channel(locator: &ChannelLocator, secret_key: &Path) -> Result<()> {
    let workdir = tempfile::tempdir().context("tempdir")?;
    let mut manifest = new_empty(locator);
    publish(locator, &mut manifest, secret_key, workdir.path())
}

fn do_show(locator: &ChannelLocator) -> Result<()> {
    let manifest = fetch_or_init(locator)?;
    let s = serde_json::to_string_pretty(&manifest).context("serialize")?;
    println!("{s}");
    Ok(())
}

fn do_image(locator: &ChannelLocator, secret_key: &Path, args: ImageArgs) -> Result<()> {
    // Compute integrity for the content blob.
    let content_bytes =
        fs::read(&args.content).with_context(|| format!("read {}", args.content.display()))?;
    let sha256 = hash_hex(&content_bytes);
    let size_bytes = content_bytes.len() as u64;
    drop(content_bytes); // free the buffer before we upload again

    // Upload the pair under images/<name>/<stamp>.{json,zfs.gz}.
    let manta_dir = format!("{}/images/{}", locator.manta_base, args.name);
    let manifest_remote = format!("{manta_dir}/{}.json", args.stamp);
    let content_remote = format!("{manta_dir}/{}.zfs.gz", args.stamp);
    mput(&args.manifest, &manifest_remote)?;
    mput(&args.content, &content_remote)?;

    let manifest_url = url::Url::parse(&format!(
        "{}/images/{}/{}.json",
        locator.https_base, args.name, args.stamp
    ))?;
    let content_url = url::Url::parse(&format!(
        "{}/images/{}/{}.zfs.gz",
        locator.https_base, args.name, args.stamp
    ))?;

    let entry = ImageEntry {
        stamp: args.stamp.clone(),
        uuid: args.uuid,
        manifest_url,
        content_url,
        sha256,
        size_bytes,
        pi_min: args.pi_min,
        data_format_version: args.data_format_version,
        data_format_min_read: args.data_format_min_read,
    };

    let mut manifest: ChannelManifest = fetch_or_init(locator)?;
    manifest.images.insert(args.name.clone(), entry);

    let workdir = tempfile::tempdir().context("tempdir")?;
    publish(locator, &mut manifest, secret_key, workdir.path())?;
    info!(name = %args.name, stamp = %args.stamp, channel = %locator.channel, "image published");
    Ok(())
}

fn do_agent(locator: &ChannelLocator, secret_key: &Path, args: AgentArgs) -> Result<()> {
    let bytes =
        fs::read(&args.tarball).with_context(|| format!("read {}", args.tarball.display()))?;
    let sha256 = hash_hex(&bytes);
    let size_bytes = bytes.len() as u64;
    drop(bytes);

    let remote = format!(
        "{}/agents/{}/{}.tar.gz",
        locator.manta_base, args.name, args.stamp
    );
    mput(&args.tarball, &remote)?;

    let url = url::Url::parse(&format!(
        "{}/agents/{}/{}.tar.gz",
        locator.https_base, args.name, args.stamp
    ))?;

    let entry = AgentEntry {
        stamp: args.stamp.clone(),
        url,
        sha256,
        size_bytes,
        pi_min: args.pi_min,
    };

    let mut manifest: ChannelManifest = fetch_or_init(locator)?;
    manifest.agents.insert(args.name.clone(), entry);

    let workdir = tempfile::tempdir().context("tempdir")?;
    publish(locator, &mut manifest, secret_key, workdir.path())?;
    info!(name = %args.name, stamp = %args.stamp, channel = %locator.channel, "agent published");
    Ok(())
}

fn do_service(locator: &ChannelLocator, secret_key: &Path, args: ServiceArgs) -> Result<()> {
    let bytes =
        fs::read(&args.binary).with_context(|| format!("read {}", args.binary.display()))?;
    let sha256 = hash_hex(&bytes);
    let size_bytes = bytes.len() as u64;
    drop(bytes);

    let remote = format!(
        "{}/services/{}/{}.bin",
        locator.manta_base, args.name, args.stamp
    );
    mput(&args.binary, &remote)?;

    let url = url::Url::parse(&format!(
        "{}/services/{}/{}.bin",
        locator.https_base, args.name, args.stamp
    ))?;

    let entry = ServiceEntry {
        stamp: args.stamp.clone(),
        url,
        sha256,
        size_bytes,
        zone: args.zone,
        bin_path: args.bin_path,
        smf: args.smf,
        pi_min: args.pi_min,
    };

    let mut manifest: ChannelManifest = fetch_or_init(locator)?;
    manifest.services.insert(args.name.clone(), entry);

    let workdir = tempfile::tempdir().context("tempdir")?;
    publish(locator, &mut manifest, secret_key, workdir.path())?;
    info!(name = %args.name, stamp = %args.stamp, channel = %locator.channel, "service published");
    Ok(())
}

fn do_tcadm(locator: &ChannelLocator, secret_key: &Path, args: TcadmArgs) -> Result<()> {
    let bytes =
        fs::read(&args.tarball).with_context(|| format!("read {}", args.tarball.display()))?;
    let sha256 = hash_hex(&bytes);
    let size_bytes = bytes.len() as u64;
    drop(bytes);

    let remote = format!(
        "{}/tcadm/{}-{}.tar.gz",
        locator.manta_base, args.stamp, args.target
    );
    mput(&args.tarball, &remote)?;

    let url = url::Url::parse(&format!(
        "{}/tcadm/{}-{}.tar.gz",
        locator.https_base, args.stamp, args.target
    ))?;

    let entry = TcadmEntry {
        stamp: args.stamp.clone(),
        url,
        sha256,
        size_bytes,
    };

    let mut manifest: ChannelManifest = fetch_or_init(locator)?;
    manifest.tcadm.insert(args.target.clone(), entry);

    let workdir = tempfile::tempdir().context("tempdir")?;
    publish(locator, &mut manifest, secret_key, workdir.path())?;
    info!(target = %args.target, stamp = %args.stamp, channel = %locator.channel, "tcadm published");
    Ok(())
}

fn hash_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn expand_tilde(p: &Path) -> Result<PathBuf> {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME").context("HOME not set, cannot expand ~")?;
        Ok(PathBuf::from(home).join(rest))
    } else {
        Ok(p.to_path_buf())
    }
}
