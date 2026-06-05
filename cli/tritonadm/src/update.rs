// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonadm update` — update running components from the signed channel.
//!
//! Hybrid model:
//!
//! - **tritonadm itself** — delegates to the existing `self-update` (atomic
//!   binary swap of the running CLI).
//! - **Zone-resident services** (`tritond`, `admin-backend`/adminui) —
//!   BINARY-SWAP: fetch the signed `services.<name>` binary, drop it into
//!   the target zone, `svcadm disable -s` / swap / `enable`. No image
//!   reprovision, so `/data` and the rest of the zone root are untouched.
//!   Fast path for the things that iterate.
//! - **GZ agents** (`tritonagent`, `proteusadm`) — re-extract the signed
//!   agent tarball (idempotent on stamp) and restart the service.
//! - **Data-bearing zones** (`fdb`, `clickhouse`, `mantad`) —
//!   REPROVISION: `imgadm install` the new image + `vmadm reprovision`,
//!   which swaps the immutable root but keeps the delegate `/data`
//!   dataset. Gated on the channel's `data_format_min_read` vs the zone's
//!   on-disk `/data/version` so an update never crosses a data-format gap
//!   (those go through the dedicated migration runbook).
//!
//! `--check` reports what is outdated; `--dry-run` prints the planned
//! actions; `--all` walks every known component (skipping any absent from
//! the channel or not provisioned on this host).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

use triton_channel::ChannelManifest;

pub struct UpdateOpts {
    pub components: Vec<String>,
    pub all: bool,
    pub check: bool,
    pub dry_run: bool,
    pub channel_url: Option<String>,
}

enum Target {
    /// `tritonadm` itself — delegate to the existing self-update flow.
    SelfTritonadm,
    /// Binary-swap a zone-resident service (`manifest.services[key]`).
    Service(&'static str),
    /// Re-extract a GZ agent tarball + restart its SMF service.
    Agent {
        key: &'static str,
        smf: Option<&'static str>,
    },
    /// Reprovision a zone to a new image (`manifest.images[image]`).
    Image {
        image: &'static str,
        zone: &'static str,
    },
}

fn resolve(name: &str) -> Option<Target> {
    Some(match name {
        "tritonadm" => Target::SelfTritonadm,
        "tritond" => Target::Service("tritond"),
        "adminui" | "admin-backend" | "ui" => Target::Service("admin-backend"),
        "tritonagent" | "agent" => Target::Agent {
            key: "tritonagent",
            smf: Some("site/tritonagent"),
        },
        "proteusadm" => Target::Agent {
            key: "proteusadm",
            smf: None,
        },
        "fdb" | "triton-fdb" => Target::Image {
            image: "triton-fdb",
            zone: "triton-fdb",
        },
        "clickhouse" | "triton-clickhouse" => Target::Image {
            image: "triton-clickhouse",
            zone: "triton-clickhouse",
        },
        "mantad" | "triton-mantad" => Target::Image {
            image: "triton-mantad",
            zone: "triton-mantad",
        },
        _ => return None,
    })
}

/// Friendly names `--all` walks. tritonadm last (it replaces the running
/// binary; let the rest run on the known one first).
const ALL: &[&str] = &[
    "tritond",
    "admin-backend",
    "tritonagent",
    "proteusadm",
    "fdb",
    "clickhouse",
    "mantad",
    "tritonadm",
];

pub fn run(opts: UpdateOpts) -> Result<()> {
    let channel_url = opts
        .channel_url
        .clone()
        .unwrap_or_else(crate::install::default_channel_url);
    let manifest = crate::install::fetch_and_verify_channel(&channel_url)?;

    let names: Vec<String> = if opts.all {
        ALL.iter().map(|s| s.to_string()).collect()
    } else if opts.components.is_empty() {
        bail!(
            "usage: tritonadm update <component>... | --all\n  components: tritonadm tritond adminui tritonagent proteusadm fdb clickhouse mantad"
        );
    } else {
        opts.components.clone()
    };

    let mode = if opts.check {
        "check"
    } else if opts.dry_run {
        "dry-run"
    } else {
        "apply"
    };
    println!("== tritonadm update ({mode}) ==");

    let mut applied = 0u32;
    let mut errors = 0u32;
    for name in &names {
        let Some(target) = resolve(name) else {
            warn!("unknown component `{name}`; skipping");
            continue;
        };
        let r = match target {
            Target::SelfTritonadm => update_self(&channel_url, &opts),
            Target::Service(key) => update_service(&manifest, name, key, &opts),
            Target::Agent { key, smf } => update_agent(&manifest, name, key, smf, &opts),
            Target::Image { image, zone } => update_image(&manifest, name, image, zone, &opts),
        };
        match r {
            Ok(true) => applied += 1,
            Ok(false) => {}
            Err(e) => {
                eprintln!("  {name}: ERROR {e:#}");
                errors += 1;
            }
        }
    }

    println!("== done: {applied} updated, {errors} error(s) ==");
    if errors > 0 {
        bail!("{errors} component(s) failed to update");
    }
    Ok(())
}

// ── tritonadm self ────────────────────────────────────────────────────────

fn update_self(channel_url: &str, opts: &UpdateOpts) -> Result<bool> {
    if opts.check {
        println!(
            "  tritonadm: run `tritonadm self-update --check` (self-update replaces the running binary)"
        );
        return Ok(false);
    }
    if opts.dry_run {
        println!("  tritonadm: would self-update against {channel_url}");
        return Ok(false);
    }
    crate::self_update::run(crate::self_update::SelfUpdateOpts {
        channel_url: Some(channel_url.to_string()),
        install_dir: None,
        check: false,
    })?;
    Ok(true)
}

// ── binary-swap a zone-resident service ───────────────────────────────

fn update_service(
    m: &ChannelManifest,
    friendly: &str,
    key: &str,
    opts: &UpdateOpts,
) -> Result<bool> {
    let Some(e) = m.services.get(key) else {
        println!("  {friendly}: not in channel; skip");
        return Ok(false);
    };
    let Some(zone) = vmadm_lookup_alias(&e.zone)? else {
        println!("  {friendly}: zone {} not provisioned; skip", e.zone);
        return Ok(false);
    };
    let host_path = zone_root_path(&zone, &e.bin_path);
    if file_sha256(&host_path).ok().as_deref() == Some(e.sha256.as_str()) {
        println!("  {friendly}: already current (stamp {})", e.stamp);
        return Ok(false);
    }
    if opts.check {
        println!(
            "  {friendly}: UPDATE available -> stamp {} (binary-swap into {})",
            e.stamp, e.zone
        );
        return Ok(false);
    }
    if opts.dry_run {
        println!(
            "  {friendly}: would fetch {} -> swap {} in {} -> restart {}",
            e.url, e.bin_path, e.zone, e.smf
        );
        return Ok(false);
    }

    println!("  {friendly}: updating -> stamp {}", e.stamp);
    let bytes = crate::install::http_get(e.url.as_str())?;
    triton_channel::verify_sha256(&bytes, &e.sha256)
        .context("downloaded service binary sha256 does NOT match the channel manifest")?;
    let new_path = format!("{host_path}.new");
    fs::write(&new_path, &bytes).with_context(|| format!("writing {new_path}"))?;
    set_exec(&new_path)?;
    // Best-effort rollback copy, then stop → atomic rename → start. The
    // disable releases the running text so the rename can't ETXTBSY.
    let _ = fs::copy(&host_path, format!("{host_path}.prev"));
    zlogin(&zone, &["svcadm", "disable", "-s", &e.smf])?;
    fs::rename(&new_path, &host_path).with_context(|| format!("mv {new_path} -> {host_path}"))?;
    set_exec(&host_path)?;
    zlogin(&zone, &["svcadm", "enable", &e.smf])?;
    println!(
        "  {friendly}: swapped + restarted {} (stamp {})",
        e.smf, e.stamp
    );
    Ok(true)
}

// ── re-extract a GZ agent + restart ───────────────────────────────────

fn update_agent(
    m: &ChannelManifest,
    friendly: &str,
    key: &str,
    smf: Option<&str>,
    opts: &UpdateOpts,
) -> Result<bool> {
    let Some(e) = m.agents.get(key) else {
        println!("  {friendly}: not in channel; skip");
        return Ok(false);
    };
    let installed = crate::install::read_installed_agent_version(key).ok();
    if installed.as_deref() == Some(e.stamp.as_str()) {
        println!("  {friendly}: already current (stamp {})", e.stamp);
        return Ok(false);
    }
    if opts.check {
        println!(
            "  {friendly}: UPDATE available -> stamp {} (was {})",
            e.stamp,
            installed.as_deref().unwrap_or("absent")
        );
        return Ok(false);
    }
    if opts.dry_run {
        println!(
            "  {friendly}: would re-install agent tarball -> stamp {}{}",
            e.stamp,
            smf.map(|s| format!(" + restart {s}")).unwrap_or_default()
        );
        return Ok(false);
    }

    crate::install::install_agent(key, e)?;
    if let Some(s) = smf {
        // Re-extract leaves the new binary on disk; restart so the running
        // process picks it up.
        let status = Command::new("svcadm").args(["restart", s]).status();
        if matches!(status, Ok(st) if st.success()) {
            println!("  {friendly}: restarted {s}");
        }
    }
    Ok(true)
}

// ── reprovision a data-bearing zone ───────────────────────────────────

fn update_image(
    m: &ChannelManifest,
    friendly: &str,
    image: &str,
    zone: &str,
    opts: &UpdateOpts,
) -> Result<bool> {
    let Some(e) = m.images.get(image) else {
        println!("  {friendly}: not in channel; skip");
        return Ok(false);
    };
    let Some(zuuid) = vmadm_lookup_alias(zone)? else {
        println!("  {friendly}: zone {zone} not provisioned; skip");
        return Ok(false);
    };
    if vmadm_zone_image(&zuuid)?.as_deref() == Some(e.uuid.to_string().as_str()) {
        println!(
            "  {friendly}: already on image {} (stamp {})",
            e.uuid, e.stamp
        );
        return Ok(false);
    }
    // Data-format gate: the existing on-disk data must be readable by the
    // new image, or reprovision can't attach.
    if let Some(on_disk) = zone_data_version(&zuuid)
        && on_disk < e.data_format_min_read
    {
        bail!(
            "refusing reprovision: zone {zone} data_format={on_disk} < new image data_format_min_read={} \
             (crosses a data-format gap — use the dedicated migration runbook)",
            e.data_format_min_read
        );
    }
    if opts.check {
        println!(
            "  {friendly}: UPDATE available -> reprovision {zone} to image {} (stamp {})",
            e.uuid, e.stamp
        );
        return Ok(false);
    }
    if opts.dry_run {
        println!(
            "  {friendly}: would imgadm install {} + vmadm reprovision {zuuid} {}",
            e.uuid, e.uuid
        );
        return Ok(false);
    }

    crate::install::install_image(image, e)?;
    println!("  {friendly}: vmadm reprovision {zuuid} -> {}", e.uuid);
    let status = Command::new("vmadm")
        .args(["reprovision", &zuuid.to_string(), &e.uuid.to_string()])
        .status()
        .context("spawning vmadm reprovision")?;
    if !status.success() {
        bail!("vmadm reprovision exited {status}");
    }
    println!("  {friendly}: reprovisioned (stamp {})", e.stamp);
    Ok(true)
}

// ── GZ helpers ────────────────────────────────────────────────────────

fn vmadm_lookup_alias(alias: &str) -> Result<Option<Uuid>> {
    let out = Command::new("vmadm")
        .args(["lookup", "-H", "-o", "uuid", &format!("alias={alias}")])
        .output()
        .context("vmadm lookup")?;
    if !out.status.success() {
        bail!("vmadm lookup exited {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| Uuid::parse_str(s).ok()))
}

fn vmadm_zone_image(zone: &Uuid) -> Result<Option<String>> {
    let out = Command::new("vmadm")
        .args(["get", &zone.to_string()])
        .output()
        .context("vmadm get")?;
    if !out.status.success() {
        return Ok(None);
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing vmadm get json")?;
    Ok(v.get("image_uuid")
        .and_then(|x| x.as_str())
        .map(str::to_string))
}

fn zone_data_version(zone: &Uuid) -> Option<u32> {
    let out = Command::new("zlogin")
        .arg(zone.to_string())
        .args(["cat", "/data/version"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn zlogin(zone: &Uuid, args: &[&str]) -> Result<()> {
    let status = Command::new("zlogin")
        .arg(zone.to_string())
        .args(args)
        .status()
        .context("zlogin")?;
    if !status.success() {
        bail!("zlogin {zone} {args:?} exited {status}");
    }
    Ok(())
}

fn zone_root_path(zone: &Uuid, bin_path: &str) -> String {
    // bin_path is absolute (`/opt/...`); join under the zone root.
    format!("/zones/{zone}/root{bin_path}")
}

fn file_sha256(path: &str) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {path}"))?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

fn set_exec(path: &str) -> Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).with_context(|| format!("chmod {path}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_maps_components_to_kinds() {
        assert!(matches!(resolve("tritonadm"), Some(Target::SelfTritonadm)));
        assert!(matches!(
            resolve("tritond"),
            Some(Target::Service("tritond"))
        ));
        assert!(matches!(
            resolve("adminui"),
            Some(Target::Service("admin-backend"))
        ));
        assert!(matches!(
            resolve("admin-backend"),
            Some(Target::Service("admin-backend"))
        ));
        assert!(matches!(
            resolve("tritonagent"),
            Some(Target::Agent {
                key: "tritonagent",
                smf: Some("site/tritonagent")
            })
        ));
        assert!(matches!(
            resolve("proteusadm"),
            Some(Target::Agent {
                key: "proteusadm",
                smf: None
            })
        ));
        assert!(matches!(
            resolve("fdb"),
            Some(Target::Image {
                zone: "triton-fdb",
                ..
            })
        ));
        assert!(matches!(
            resolve("mantad"),
            Some(Target::Image {
                image: "triton-mantad",
                ..
            })
        ));
        assert!(resolve("nope").is_none());
    }

    #[test]
    fn all_set_resolves_and_covers_user_asks() {
        for n in ALL {
            assert!(resolve(n).is_some(), "ALL entry {n} must resolve");
        }
        for n in ["tritond", "tritonagent", "admin-backend", "tritonadm"] {
            assert!(ALL.contains(&n), "{n} should be in --all");
        }
    }

    #[test]
    fn zone_root_path_joins_under_zone_root() {
        let z = Uuid::from_u128(0xabc);
        assert_eq!(
            zone_root_path(&z, "/opt/triton/tritond/bin/tritond"),
            format!("/zones/{z}/root/opt/triton/tritond/bin/tritond")
        );
    }

    #[test]
    fn file_sha256_matches_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x");
        fs::write(&p, b"abc").unwrap();
        // sha256("abc")
        assert_eq!(
            file_sha256(p.to_str().unwrap()).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
