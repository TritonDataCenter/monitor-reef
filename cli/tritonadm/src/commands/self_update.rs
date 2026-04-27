// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonadm self-update` — download + exec the latest (or pinned)
//! tritonadm installer shar from the updates channel.
//!
//! Mirrors the flow sdcadm's `experimental get-tritonadm` uses
//! (TritonDataCenter/sdcadm#112): both tools fetch the same image
//! artifact, both read /opt/triton/tritonadm/etc/version for the
//! "Already up-to-date" short-circuit, and both exec the shar
//! directly — the shar's own install.sh writes the new etc/version
//! after successful extraction.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use futures_util::TryStreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

use imgapi_client::Client;

/// Key=value file install.sh writes. sdcadm's get-tritonadm reads
/// this same path — see TRITONADM_VERSION_FILE there.
const VERSION_FILE: &str = "/opt/triton/tritonadm/etc/version";

/// Where to stage the downloaded shar before exec. Matches sdcadm's
/// INSTALLER_DIR so both tools touch the same place on the GZ.
const INSTALLER_DIR: &str = "/var/tmp";

/// Advisory flock(2) path. Serializes concurrent `tritonadm self-update`
/// (and `tritonadm self-update` racing with another orchestrator calling
/// the shar). `/var/run` is tmpfs on the GZ; the lock file is effectively
/// a no-op persisted across boots.
const LOCK_FILE: &str = "/var/run/tritonadm-self-update.lock";

/// Parent of the per-run workdir where we capture installer stdout/stderr
/// to install.log. Matches sdcadm's /var/sdcadm/self-updates/<stamp>/
/// convention, but under /var/log so log-rotation can own it.
const LOG_ROOT: &str = "/var/log/tritonadm-self-updates";

pub struct SelfUpdateOpts {
    pub updates_url: String,
    /// Optional — None means "auto-detect". Passed through to
    /// post_setup::resolve_channel for flag > SAPI sdc.metadata >
    /// updates-server-default fallback (matches sdcadm).
    pub channel: Option<String>,
    /// Optional — only needed if channel is None (to look up the
    /// sdc application's update_channel metadata). Usually the
    /// SDC-config auto-detected URL on a headnode.
    pub sapi_url: Option<String>,
    /// None means "pick the latest on the channel"; Some(uuid) pins.
    pub image_uuid: Option<Uuid>,
    /// When true, set TRACE=1 on the installer exec so the shar
    /// emits full xtrace. sdcadm's get-tritonadm does this
    /// unconditionally since it captures output to a log file; we
    /// default off for interactive UX and opt in via --verbose.
    pub verbose: bool,
    /// Dry-run: resolve channel/installed/candidate + print, but
    /// skip lock acquisition, download, and installer exec. Matches
    /// sdcadm's self-update -n (and sdcadm's get-tritonadm is also
    /// lock-skipping in dry-run, so concurrent dry-runs don't fight).
    pub dry_run: bool,
}

pub async fn run(opts: SelfUpdateOpts) -> Result<()> {
    let dry_prefix = if opts.dry_run { "[dry-run] " } else { "" };

    // Fail fast if we're obviously not on a Triton headnode GZ. sdcadm's
    // shar does the equivalent check in its preamble (sdcadm/tools/mk-shar);
    // we do it here so the error message is "self-update must run on a
    // headnode GZ" rather than a confusing downstream write failure.
    require_headnode_gz()?;

    // Serialize self-update invocations. Dry-run skips this so a
    // concurrent real run doesn't block an operator from sanity-
    // checking the channel — same behavior as sdcadm's self-update
    // (sdcadm/lib/cli/do_get_tritonadm.js getLock:168).
    let _lock = if opts.dry_run {
        None
    } else {
        Some(acquire_self_update_lock().context("self-update lock")?)
    };

    let http = triton_tls::build_http_client(false)
        .await
        .context("failed to build HTTP client")?;
    let updates = Client::new_with_client(&opts.updates_url, http);

    // Resolve channel: --channel flag > sdc SAPI metadata > updates default.
    // Mirrors sdcadm's getDefaultChannel.
    let channel = if let Some(ch) = opts.channel.clone() {
        ch
    } else {
        let sapi_url = opts.sapi_url.as_ref().ok_or_else(|| {
            anyhow!(
                "cannot determine update channel: pass --channel explicitly, \
                 or run on a Triton headnode where the sdc app's SAPI \
                 metadata supplies update_channel"
            )
        })?;
        let sapi = sapi_client::build_client(sapi_url, false)
            .await
            .context("failed to build SAPI client")?;
        let apps = sapi
            .list_applications()
            .name("sdc")
            .send()
            .await
            .context("failed to list sdc application")?
            .into_inner();
        let sdc_app = apps
            .first()
            .ok_or_else(|| anyhow!("no 'sdc' application found in SAPI"))?;
        let sdc_metadata = sdc_app
            .metadata
            .as_ref()
            .ok_or_else(|| anyhow!("sdc application has no metadata"))?;
        super::post_setup::resolve_channel(None, sdc_metadata, &updates).await?
    };

    println!("Using channel {channel}");

    let installed = read_installed_version(VERSION_FILE).await;
    match &installed {
        Some(v) => println!(
            "Installed tritonadm: uuid={} version={}",
            v.get("uuid").map(String::as_str).unwrap_or("<unknown>"),
            v.get("version").map(String::as_str).unwrap_or("<unknown>"),
        ),
        None => println!("No tritonadm currently installed"),
    }

    let candidate = match opts.image_uuid {
        Some(uuid) => updates
            .get_image()
            .uuid(uuid)
            .channel(channel.clone())
            .send()
            .await
            .with_context(|| format!("failed to fetch image {uuid}"))?
            .into_inner(),
        None => {
            let images = updates
                .list_images()
                .name("tritonadm")
                .state("active")
                .channel(channel.clone())
                .send()
                .await
                .context("failed to list tritonadm images")?
                .into_inner();
            images
                .into_iter()
                .max_by(|a, b| a.published_at.cmp(&b.published_at))
                .ok_or_else(|| {
                    anyhow!(
                        "no active tritonadm images on channel \"{}\" at {}",
                        channel,
                        opts.updates_url,
                    )
                })?
        }
    };

    // Short-circuit if the installed image UUID matches what we'd
    // download. sdcadm uses the same comparison.
    let installed_uuid = installed.as_ref().and_then(|v| v.get("uuid"));
    if installed_uuid.map(String::as_str) == Some(candidate.uuid.to_string().as_str()) {
        println!("Already up-to-date (using \"{channel}\" update channel).");
        return Ok(());
    }

    println!(
        "{dry_prefix}Install tritonadm {} ({})",
        candidate.version, candidate.uuid,
    );

    if opts.dry_run {
        return Ok(());
    }

    println!("Download tritonadm image from {}", opts.updates_url);

    let installer_path = format!("{}/tritonadm-{}", INSTALLER_DIR, candidate.uuid);
    let resp = updates
        .get_image_file()
        .uuid(candidate.uuid)
        .channel(channel.clone())
        .send()
        .await
        .with_context(|| format!("failed to download {}", candidate.uuid))?;
    let chunks: Vec<bytes::Bytes> = resp
        .into_inner()
        .into_inner()
        .try_collect()
        .await
        .context("failed reading image bytes")?;
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    let mut data = Vec::with_capacity(total);
    for chunk in chunks {
        data.extend_from_slice(&chunk);
    }
    tokio::fs::write(&installer_path, &data)
        .await
        .with_context(|| format!("failed to write {installer_path}"))?;
    let mut perms = tokio::fs::metadata(&installer_path).await?.permissions();
    perms.set_mode(0o755);
    tokio::fs::set_permissions(&installer_path, perms).await?;

    // Spawn the installer and tee its stdout/stderr to both our tty
    // and /var/log/tritonadm-self-updates/<stamp>/install.log. We used
    // to exec() into the shar, but that lost the audit trail and made
    // the command unsuitable for automation — matches sdcadm's
    // workdir + install.log pattern now.
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let workdir = PathBuf::from(LOG_ROOT).join(&stamp);
    tokio::fs::create_dir_all(&workdir)
        .await
        .with_context(|| format!("failed to create workdir {}", workdir.display()))?;
    let log_path = workdir.join("install.log");
    println!("Run tritonadm installer (log at {})", log_path.display());
    let status = run_installer_with_tee(&installer_path, &log_path, opts.verbose)
        .await
        .with_context(|| format!("failed to run installer {installer_path}"))?;
    if !status.success() {
        anyhow::bail!(
            "tritonadm installer exited with {status}; \
             see {}",
            log_path.display(),
        );
    }
    Ok(())
}

/// Spawn the installer with its stdout+stderr piped through us. Each
/// line we read is written to the operator's terminal AND appended to
/// install.log, so an automation caller can review the full output and
/// an interactive operator sees progress in real time. Returns the
/// exit status; the caller decides how to react to non-zero.
async fn run_installer_with_tee(
    installer: &str,
    log_path: &std::path::Path,
    verbose: bool,
) -> Result<std::process::ExitStatus> {
    let log_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    let log = std::sync::Arc::new(tokio::sync::Mutex::new(log_file));

    let mut cmd = tokio::process::Command::new(installer);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // No stdin — the installer shouldn't prompt.
        .stdin(Stdio::null());
    if verbose {
        cmd.env("TRACE", "1");
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {installer}"))?;

    let stdout = child.stdout.take().context("child stdout unavailable")?;
    let stderr = child.stderr.take().context("child stderr unavailable")?;

    let stdout_task = tokio::spawn(tee_lines(stdout, log.clone(), /*to_stderr=*/ false));
    let stderr_task = tokio::spawn(tee_lines(stderr, log, /*to_stderr=*/ true));

    let status = child.wait().await.context("waiting for installer")?;
    // Best-effort: ensure any residual buffered output is drained
    // before we return; errors here are advisory only.
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    Ok(status)
}

/// Read lines from `reader`, appending each to the shared log file
/// and echoing to our stdout or stderr. stderr lines go to stderr so
/// the operator sees red-flag messages in their usual place; both
/// streams end up interleaved in install.log by order of arrival.
async fn tee_lines<R>(
    reader: R,
    log: std::sync::Arc<tokio::sync::Mutex<tokio::fs::File>>,
    to_stderr: bool,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = buf
            .read_until(b'\n', &mut line)
            .await
            .context("reading installer output")?;
        if n == 0 {
            return Ok(());
        }
        if to_stderr {
            let mut stderr = tokio::io::stderr();
            stderr.write_all(&line).await.ok();
        } else {
            let mut stdout = tokio::io::stdout();
            stdout.write_all(&line).await.ok();
        }
        let mut log = log.lock().await;
        log.write_all(&line).await.ok();
    }
}

/// Check that we're running in the global zone on a Triton headnode.
/// Same posture as sdcadm's shar (zonename=global, sysinfo says
/// Boot Parameters.headnode=true). On non-illumos hosts the zonename
/// binary is missing and we bail with the same "not a headnode"
/// error — self-update is definitionally a headnode-GZ operation.
fn require_headnode_gz() -> Result<()> {
    let out = Command::new("zonename").output().map_err(|e| {
        anyhow!(
            "self-update requires a Triton headnode global zone \
             (zonename(1) not available: {e})"
        )
    })?;
    if !out.status.success() {
        anyhow::bail!(
            "zonename failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let zone = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if zone != "global" {
        anyhow::bail!("self-update must run in the global zone (currently in zone \"{zone}\")");
    }

    let out = Command::new("sysinfo").output().map_err(|e| {
        anyhow!(
            "self-update requires a Triton headnode (sysinfo(8) \
             not available: {e})"
        )
    })?;
    if !out.status.success() {
        anyhow::bail!(
            "sysinfo failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let info: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("failed to parse sysinfo JSON")?;
    let headnode = info
        .get("Boot Parameters")
        .and_then(|bp| bp.get("headnode"))
        .and_then(|v| v.as_str());
    if headnode != Some("true") {
        anyhow::bail!(
            "self-update must run on a Triton headnode \
             (sysinfo \"Boot Parameters.headnode\" is {headnode:?})"
        );
    }
    Ok(())
}

/// flock(LOCK_EX|LOCK_NB) on LOCK_FILE. Returns the open file so the
/// caller can hold the lock for the duration of the update (dropping
/// the file closes the fd, which releases the flock). Fails fast with
/// a clear message if another self-update is already running.
fn acquire_self_update_lock() -> Result<File> {
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(LOCK_FILE)
        .with_context(|| format!("failed to open lockfile {LOCK_FILE}"))?;
    // SAFETY: libc::flock takes a raw fd and integer flags. fd is
    // guaranteed valid until `f` is dropped.
    let ret = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            anyhow::bail!(
                "another `tritonadm self-update` is already running \
                 (holds lock {LOCK_FILE})"
            );
        }
        return Err(err).with_context(|| format!("flock({LOCK_FILE}) failed"));
    }
    Ok(f)
}

/// Parse the KEY=VALUE file install.sh writes. Returns None on missing
/// file or when uuid= isn't present (treat as "no tritonadm installed").
async fn read_installed_version(path: &str) -> Option<HashMap<String, String>> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    if map.contains_key("uuid") {
        Some(map)
    } else {
        None
    }
}
