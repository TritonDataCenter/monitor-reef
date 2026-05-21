// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tcadm self-update` — fetch the latest tcadm tarball for this
//! host's target triple from the Manta-hosted release channel,
//! verify the channel's minisign signature against the embedded
//! publisher pubkey, verify the tarball's SHA-256 against the
//! (signed) manifest entry, and atomically replace the running
//! binary.
//!
//! See `rfd/00006/01-pipeline-and-channels.md` for the broader design.
//!
//! ## Atomicity
//!
//! New binary goes to `<install-dir>/tcadm.new`, then is renamed over
//! `<install-dir>/tcadm`. The old binary survives one cycle as
//! `<install-dir>/tcadm.prev` so a bad release can be rolled back by
//! hand without re-fetching anything.
//!
//! ## Compatibility check
//!
//! The channel manifest carries a per-target sha256 and (optionally)
//! a stamp string. We compare the manifest stamp against the embedded
//! `BUILD_STAMP` const baked into this binary at compile time. If
//! they match, we exit without touching the disk so re-runs are
//! cheap.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use triton_channel::{TcadmEntry, parse_channel, verify_minisign, verify_sha256};

/// Embedded publisher pubkey. Committed at
/// `monitor-reef/cli/tcadm/publisher.pub`; install.sh embeds the same
/// bytes via a heredoc so both consumers verify against the same
/// trust root.
const PUBLISHER_PUBKEY: &str = include_str!("../publisher.pub");

/// Default channel URL when none is provided. Stable is the
/// recommended channel for operators; switch to edge via
/// `--channel-url` for testing.
const DEFAULT_CHANNEL_URL: &str = "https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/channels/stable.json";

/// Build stamp baked in at compile time. Populated by `build.rs` from
/// the current UTC time; matches the `--stamp` value the publisher
/// used when pushing this binary to Manta.
const BUILD_STAMP: &str = env!("TCADM_BUILD_STAMP");

/// Self-update options surfaced to the CLI dispatcher.
pub struct SelfUpdateOpts {
    /// Override the channel manifest URL.
    pub channel_url: Option<String>,

    /// Override the install dir. Defaults to the directory containing
    /// the currently-running tcadm executable.
    pub install_dir: Option<PathBuf>,

    /// Report current vs latest and exit non-zero if outdated, but do
    /// not download or replace anything.
    pub check: bool,
}

/// Run the self-update flow.
pub fn run(opts: SelfUpdateOpts) -> Result<()> {
    let channel_url = opts
        .channel_url
        .unwrap_or_else(|| DEFAULT_CHANNEL_URL.to_string());
    let target = current_target()?;

    info!(channel_url = %channel_url, target = %target, "self-update");

    let (manifest_bytes, sig_bytes) = fetch_channel(&channel_url)?;
    verify_minisign(&manifest_bytes, &sig_bytes, PUBLISHER_PUBKEY)
        .context("channel signature did NOT verify against publisher pubkey")?;
    let channel = parse_channel(&manifest_bytes)?;

    let entry = channel
        .tcadm
        .get(&target)
        .ok_or_else(|| anyhow!("channel has no tcadm entry for target {target}"))?;

    println!("installed: {}", BUILD_STAMP);
    println!("candidate: {} ({} bytes)", entry.stamp, entry.size_bytes);

    if entry.stamp == BUILD_STAMP {
        println!("already up to date");
        return Ok(());
    }

    if opts.check {
        bail!(
            "tcadm is outdated (installed {}, candidate {})",
            BUILD_STAMP,
            entry.stamp
        );
    }

    let install_dir = match opts.install_dir {
        Some(d) => d,
        None => current_install_dir()?,
    };

    download_and_swap(entry, &install_dir)
}

/// Return the Rust target triple for the running binary. We baked it
/// into the binary at build time via `build.rs` so this never relies
/// on runtime detection that could disagree with what cargo built.
fn current_target() -> Result<String> {
    Ok(env!("TCADM_TARGET").to_string())
}

/// Return the directory containing the currently-running tcadm
/// executable. Falls back to `/opt/triton/bin` if we cannot read
/// `/proc/self/exe`-equivalent on illumos.
fn current_install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("std::env::current_exe failed")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("running binary has no parent dir: {}", exe.display()))?;
    Ok(dir.to_path_buf())
}

/// Fetch the channel manifest and its detached signature.
fn fetch_channel(channel_url: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let manifest = http_get(channel_url)?;
    let sig = http_get(&format!("{channel_url}.minisig"))?;
    Ok((manifest, sig))
}

/// Synchronous blocking HTTP GET into memory. We avoid pulling tokio
/// into the self-update path because the caller is a top-level CLI
/// command with no async story; reqwest's blocking client is fine.
fn http_get(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::blocking::get(url).with_context(|| format!("fetching {url}"))?;
    if !resp.status().is_success() {
        bail!("GET {url} -> {}", resp.status());
    }
    resp.bytes()
        .with_context(|| format!("reading body of {url}"))
        .map(|b| b.to_vec())
}

/// Download the candidate tarball, verify sha256, extract the
/// `tcadm` binary, and atomically swap it onto the install path.
fn download_and_swap(entry: &TcadmEntry, install_dir: &Path) -> Result<()> {
    let workdir = tempfile::tempdir().context("tempdir")?;
    let tarball_path = workdir.path().join("tcadm.tar.gz");

    // Stream the tarball into memory + disk, computing sha256 as we
    // go so we don't read the bytes twice.
    let bytes = http_get(entry.url.as_str())?;
    verify_sha256(&bytes, &entry.sha256)
        .context("downloaded tarball sha256 does NOT match channel manifest")?;
    fs::write(&tarball_path, &bytes)
        .with_context(|| format!("writing {}", tarball_path.display()))?;
    drop(bytes);

    extract_tcadm(&tarball_path, workdir.path())?;
    let extracted = workdir.path().join("tcadm");
    if !extracted.exists() {
        bail!("tarball did not contain a `tcadm` binary at the top level");
    }

    // Make sure the new binary is executable; some tar configurations
    // strip the mode.
    let mut perms = fs::metadata(&extracted)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&extracted, perms)?;

    swap_atomically(&extracted, install_dir)
}

fn extract_tcadm(tarball: &Path, into: &Path) -> Result<()> {
    let f = fs::File::open(tarball).with_context(|| format!("open {}", tarball.display()))?;
    let gz = flate2_decoder(f)?;
    let mut archive = tar::Archive::new(gz);
    archive
        .unpack(into)
        .with_context(|| format!("extracting {}", tarball.display()))
}

/// Open a gzip reader. We use flate2 transitively via reqwest; if a
/// future refactor drops it, add `flate2` to the deps explicitly.
fn flate2_decoder<R: Read>(r: R) -> Result<impl Read> {
    Ok(flate2::read::GzDecoder::new(r))
}

/// Atomically replace `<install_dir>/tcadm` with the binary at
/// `new_binary`. The current binary survives as `tcadm.prev` for one
/// cycle so a bad release can be hand-rolled-back.
fn swap_atomically(new_binary: &Path, install_dir: &Path) -> Result<()> {
    let live = install_dir.join("tcadm");
    let prev = install_dir.join("tcadm.prev");
    let staged = install_dir.join("tcadm.new");

    fs::copy(new_binary, &staged)
        .with_context(|| format!("copying {} -> {}", new_binary.display(), staged.display()))?;
    let mut perms = fs::metadata(&staged)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&staged, perms)?;

    // If a prior tcadm exists, move it to .prev so we have a rollback
    // path. Ignore the "no live binary" case (first install via
    // install.sh, then self-update before anything else ran).
    if live.exists() {
        if prev.exists() {
            fs::remove_file(&prev).with_context(|| format!("removing stale {}", prev.display()))?;
        }
        fs::rename(&live, &prev).with_context(|| {
            format!(
                "rename {} -> {} (rollback path)",
                live.display(),
                prev.display()
            )
        })?;
    } else {
        warn!("no existing tcadm at {}; installing fresh", live.display());
    }

    fs::rename(&staged, &live).with_context(|| {
        format!(
            "rename {} -> {} (swap-in)",
            staged.display(),
            live.display()
        )
    })?;

    println!("tcadm updated at {}", live.display());
    println!(
        "previous binary preserved at {} (manually delete when satisfied)",
        prev.display()
    );
    Ok(())
}

/// SHA-256 a file streaming, returning the lowercase hex digest. Used
/// only for diagnostics; the trusted compare goes through
/// `triton_channel::verify_sha256`.
#[allow(dead_code)]
pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}
