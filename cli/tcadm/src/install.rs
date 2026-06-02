// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tcadm install` — fetch and install artifacts from the Manta
//! release channel.
//!
//! One verb covers both kinds:
//!
//! - `tcadm install <name>` resolves `<name>` in the channel
//!   manifest. If it appears under `images`, the artifact is a
//!   SmartOS zone-dataset image and we drive `imgadm install`. If it
//!   appears under `agents`, it's a GZ tarball and we extract at `/`,
//!   `svccfg import` any new manifests, and `svcadm enable` any
//!   newly-imported site/<name> services.
//!
//! - `tcadm install --list` enumerates everything in the channel
//!   alongside what's currently installed on this host (for images:
//!   `imgadm list`; for agents: `/opt/triton/<name>/etc/version`).
//!
//! The implementation deliberately keeps imgadm and svccfg as
//! shell-outs rather than reimplementing them; both are part of the
//! illumos / SmartOS surface and stable enough to trust.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use tracing::info;
use triton_channel::{
    AgentEntry, ChannelManifest, ImageEntry, parse_channel, verify_minisign, verify_sha256,
};

/// Embedded publisher pubkey. Same trust root as `tcadm self-update`.
const PUBLISHER_PUBKEY: &str = include_str!("../publisher.pub");

/// Default channel URL when `--channel-url` is not given. Operators
/// install from stable by default; `--channel-url …/edge.json` for
/// pre-release testing.
const DEFAULT_CHANNEL_URL: &str = "https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/channels/stable.json";

/// The default (stable) channel URL, shared with `tcadm setup`.
pub(crate) fn default_channel_url() -> String {
    DEFAULT_CHANNEL_URL.to_string()
}

pub struct InstallOpts {
    pub name: Option<String>,
    pub stamp: Option<String>,
    pub channel_url: Option<String>,
    pub list: bool,
}

pub fn run(opts: InstallOpts) -> Result<()> {
    let channel_url = opts
        .channel_url
        .unwrap_or_else(|| DEFAULT_CHANNEL_URL.to_string());

    let manifest = fetch_and_verify_channel(&channel_url)?;

    if opts.list {
        return show_listing(&manifest);
    }

    let name = opts
        .name
        .ok_or_else(|| anyhow!("usage: tcadm install <name> | tcadm install --list"))?;

    // Refuse to act on a channel-side pin mismatch — if the operator
    // specified --stamp, the channel entry must match. We do not
    // download arbitrary stamps that the channel doesn't currently
    // point at; the channel is the source of truth.
    if let Some(img) = manifest.images.get(&name) {
        check_stamp(opts.stamp.as_deref(), &img.stamp)?;
        return install_image(&name, img);
    }
    if let Some(agent) = manifest.agents.get(&name) {
        check_stamp(opts.stamp.as_deref(), &agent.stamp)?;
        return install_agent(&name, agent);
    }
    bail!(
        "no image or agent named `{name}` in channel {channel_url}. \
         try `tcadm install --list` to see what is available."
    )
}

fn check_stamp(requested: Option<&str>, channel_stamp: &str) -> Result<()> {
    if let Some(want) = requested
        && want != channel_stamp
    {
        bail!(
            "channel currently points at stamp `{channel_stamp}` but --stamp asked for `{want}`. \
             Either drop --stamp (to take whatever the channel points at now) or republish/promote \
             the desired stamp into the channel first."
        );
    }
    Ok(())
}

pub(crate) fn fetch_and_verify_channel(channel_url: &str) -> Result<ChannelManifest> {
    info!(channel_url = %channel_url, "fetch channel");
    let manifest_bytes = http_get(channel_url)?;
    let sig_bytes = http_get(&format!("{channel_url}.minisig"))?;
    verify_minisign(&manifest_bytes, &sig_bytes, PUBLISHER_PUBKEY)
        .context("channel signature did NOT verify against publisher pubkey")?;
    parse_channel(&manifest_bytes).map_err(Into::into)
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    let client = crate::http::blocking_client()?;
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("fetching {url}"))?;
    if !resp.status().is_success() {
        bail!("GET {url} -> {}", resp.status());
    }
    resp.bytes()
        .with_context(|| format!("reading body of {url}"))
        .map(|b| b.to_vec())
}

fn show_listing(manifest: &ChannelManifest) -> Result<()> {
    println!("channel:    {}", manifest.channel);
    println!("updated_at: {}", manifest.updated_at);
    println!();
    println!("=== images ===");
    for (name, img) in &manifest.images {
        let installed = imgadm_has(&img.uuid.to_string()).unwrap_or(false);
        println!(
            "  {:<24}  stamp={}  uuid={}  {}",
            name,
            img.stamp,
            img.uuid,
            if installed { "[installed]" } else { "" }
        );
    }
    println!();
    println!("=== agents ===");
    for (name, agent) in &manifest.agents {
        let installed_stamp = read_installed_agent_version(name).ok();
        let status = match installed_stamp.as_deref() {
            Some(s) if s == agent.stamp => "[installed, current]".to_string(),
            Some(s) => format!("[installed {s}]"),
            None => String::new(),
        };
        println!("  {:<24}  stamp={}  {}", name, agent.stamp, status);
    }
    println!();
    println!("=== tcadm ===");
    for (target, t) in &manifest.tcadm {
        println!("  {:<32}  stamp={}", target, t.stamp);
    }
    Ok(())
}

pub(crate) fn install_image(name: &str, entry: &ImageEntry) -> Result<()> {
    if imgadm_has(&entry.uuid.to_string()).unwrap_or(false) {
        println!(
            "image {} already installed (uuid {}); nothing to do",
            name, entry.uuid
        );
        return Ok(());
    }

    let workdir = tempfile::tempdir().context("tempdir")?;
    let manifest_path = workdir.path().join(format!("{name}.json"));
    let content_path = workdir.path().join(format!("{name}.zfs.gz"));

    println!("fetching manifest from {}", entry.manifest_url);
    let m = http_get(entry.manifest_url.as_str())?;
    fs::write(&manifest_path, &m)
        .with_context(|| format!("writing {}", manifest_path.display()))?;

    println!(
        "fetching content from {} ({} bytes)",
        entry.content_url, entry.size_bytes
    );
    let c = http_get(entry.content_url.as_str())?;
    verify_sha256(&c, &entry.sha256)
        .context("downloaded image content sha256 does NOT match channel manifest")?;
    fs::write(&content_path, &c).with_context(|| format!("writing {}", content_path.display()))?;
    drop(c);

    println!(
        "imgadm install -m {} -f {}",
        manifest_path.display(),
        content_path.display()
    );
    let status = Command::new("imgadm")
        .arg("install")
        .arg("-m")
        .arg(&manifest_path)
        .arg("-f")
        .arg(&content_path)
        .status()
        .context("spawning imgadm install")?;
    if !status.success() {
        bail!("imgadm install exited {status}");
    }

    println!("image {} installed (uuid {})", name, entry.uuid);
    Ok(())
}

pub(crate) fn install_agent(name: &str, entry: &AgentEntry) -> Result<()> {
    let existing = read_installed_agent_version(name).ok();
    if existing.as_deref() == Some(&entry.stamp) {
        println!(
            "agent {} already at stamp {}; nothing to do",
            name, entry.stamp
        );
        return Ok(());
    }

    let workdir = tempfile::tempdir().context("tempdir")?;
    let tarball_path = workdir.path().join(format!("{name}.tar.gz"));

    println!("fetching {} ({} bytes)", entry.url, entry.size_bytes);
    let bytes = http_get(entry.url.as_str())?;
    verify_sha256(&bytes, &entry.sha256)
        .context("downloaded agent tarball sha256 does NOT match channel manifest")?;
    fs::write(&tarball_path, &bytes)
        .with_context(|| format!("writing {}", tarball_path.display()))?;
    drop(bytes);

    println!("extracting at /");
    let tar = which_tar()?;
    let status = Command::new(tar)
        .arg("-C")
        .arg("/")
        .arg("-xzf")
        .arg(&tarball_path)
        .status()
        .context("spawning tar")?;
    if !status.success() {
        bail!("tar extract exited {status}");
    }

    // Import every SMF manifest under /opt/triton/<name>/smf/ and
    // svcadm enable each service it defines. The list of services to
    // enable is `site/<basename-without-extension>` for each xml in
    // smf/; this matches the convention we use across all agents.
    let smf_dir: PathBuf = Path::new("/opt/triton").join(name).join("smf");
    let mut enabled_any = false;
    if smf_dir.is_dir() {
        for entry in
            fs::read_dir(&smf_dir).with_context(|| format!("reading {}", smf_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("xml") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("bad smf manifest name: {}", path.display()))?
                .to_string();
            println!("svccfg import {}", path.display());
            let status = Command::new("svccfg")
                .arg("import")
                .arg(&path)
                .status()
                .context("spawning svccfg")?;
            if !status.success() {
                bail!("svccfg import {} exited {status}", path.display());
            }
            let svc = format!("site/{stem}");
            println!("svcadm enable -s {svc}");
            let status = Command::new("svcadm")
                .arg("enable")
                .arg("-s")
                .arg(&svc)
                .status()
                .context("spawning svcadm enable")?;
            if !status.success() {
                bail!("svcadm enable {svc} exited {status}");
            }
            enabled_any = true;
        }
    }

    // Set executable bit on any extracted method scripts. tar
    // preserves perms when it can, but a tarball produced on macOS
    // may have stripped them.
    let method_path: PathBuf = Path::new("/var/svc/method").join(name);
    if method_path.exists() {
        let mut perms = fs::metadata(&method_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&method_path, perms)?;
    }

    println!(
        "agent {} installed at stamp {}{}",
        name,
        entry.stamp,
        if enabled_any {
            ""
        } else {
            " (no SMF service in tarball)"
        }
    );
    Ok(())
}

/// Return the installed image UUIDs (one per line) via `imgadm list -H -o uuid`,
/// and check whether `target_uuid` is in that list.
fn imgadm_has(target_uuid: &str) -> Result<bool> {
    let out = Command::new("imgadm")
        .args(["list", "-H", "-o", "uuid"])
        .output()
        .context("imgadm list")?;
    if !out.status.success() {
        bail!("imgadm list exited {}", out.status);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|line| line.trim() == target_uuid))
}

fn read_installed_agent_version(name: &str) -> Result<String> {
    let path: PathBuf = Path::new("/opt/triton").join(name).join("etc/version");
    let s = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(s.trim().to_string())
}

fn which_tar() -> Result<String> {
    for candidate in ["gtar", "tar"] {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Ok(candidate.to_string());
        }
    }
    bail!("neither gtar nor tar found in PATH")
}
