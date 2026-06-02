// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tcadm setup apply` — converged single-node headnode bring-up.
//!
//! Installs the signed zone images from the channel, `vmadm create`s the
//! control-plane zones with their first-boot metadata in dependency order
//! with health gates between them, installs the GZ agents, and surfaces
//! the one-time root password + endpoints. This is the provisioning layer
//! `tcadm install` deliberately stops short of (install only does
//! `imgadm install` / agent-tar; it never `vmadm create`s a zone).
//!
//! Sequence: triton-fdb (self-runs `fdbcli configure new single ssd`) →
//! gate on `/data/version` → triton-tritond (+ admin-backend/adminui;
//! refuses to boot without the SAME fdb secret) → gate on
//! `/v1/health` → optional triton-mantad (S3) / triton-clickhouse →
//! tritonagent (+ agent.env) / proteusadm.
//!
//! SCOPE (v1): assumes an existing SmartOS CN with the `zones` pool +
//! admin nic_tag already laid down (no zpool/dladm/ipadm here). Single
//! node only (FDB `configure new single`). Inputs come from a TOML file
//! + flags (no TUI). mantad bucket creation + tritond storage-cluster /
//! clickhouse registration are printed as follow-ups, not auto-run (they
//! need an authenticated tritond session). Re-runnable: image installs
//! short-circuit on `imgadm_has`, zone creates skip when the alias
//! already exists; the per-zone first-boot methods are themselves
//! fresh-vs-attach gated, so a re-run never re-inits FDB or re-mints
//! secrets.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};
use uuid::Uuid;

use triton_channel::ImageEntry;

const DEFAULT_NIC_TAG: &str = "admin";
const DEFAULT_NETMASK: &str = "255.255.255.0";

/// Health-gate budgets. FDB's first-boot subshell can wait ~60s before it
/// runs `configure new`, so its gate gets the most slack.
const FDB_GATE: Duration = Duration::from_secs(240);
const HTTP_GATE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(4);

pub struct SetupOpts {
    pub config: Option<String>,
    pub fdb_ip: Option<String>,
    pub tritond_ip: Option<String>,
    pub mantad_ip: Option<String>,
    pub clickhouse_ip: Option<String>,
    pub netmask: Option<String>,
    pub gateway: Option<String>,
    pub resolver: Option<String>,
    pub nic_tag: Option<String>,
    pub channel_url: Option<String>,
    pub with_mantad: bool,
    pub with_clickhouse: bool,
    pub fdb_secret: Option<String>,
    pub dry_run: bool,
}

/// On-disk `setup.toml`. Every field optional; CLI flags overlay it.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SetupConfig {
    nic_tag: Option<String>,
    netmask: Option<String>,
    gateway: Option<String>,
    resolver: Option<String>,
    fdb_ip: Option<String>,
    tritond_ip: Option<String>,
    mantad_ip: Option<String>,
    clickhouse_ip: Option<String>,
    with_mantad: Option<bool>,
    with_clickhouse: Option<bool>,
    channel_url: Option<String>,
}

/// Fully-resolved bring-up plan (config + flags merged, secret minted).
struct Plan {
    nic_tag: String,
    netmask: String,
    gateway: Option<String>,
    resolver: Option<String>,
    fdb_ip: String,
    tritond_ip: String,
    mantad_ip: Option<String>,
    clickhouse_ip: Option<String>,
    fdb_secret: String,
    channel_url: String,
    with_mantad: bool,
    with_clickhouse: bool,
    dry_run: bool,
}

pub fn run(opts: SetupOpts) -> Result<()> {
    let plan = resolve_plan(opts)?;

    println!("== tcadm setup: single-node headnode bring-up ==");
    println!("  nic_tag={} netmask={}", plan.nic_tag, plan.netmask);
    println!("  fdb={} tritond={}", plan.fdb_ip, plan.tritond_ip);
    if plan.with_mantad {
        println!(
            "  mantad={}",
            plan.mantad_ip.as_deref().unwrap_or("(missing ip)")
        );
    }
    if plan.with_clickhouse {
        println!(
            "  clickhouse={}",
            plan.clickhouse_ip.as_deref().unwrap_or("(missing ip)")
        );
    }
    if plan.dry_run {
        println!("  *** DRY RUN — printing payloads, mutating nothing ***");
    }

    let manifest = crate::install::fetch_and_verify_channel(&plan.channel_url)?;

    // ── FDB (required, first; everything blocks on it) ────────────────
    let fdb_img = require_image(&manifest.images, "triton-fdb")?;
    let fdb_uuid = ensure_zone(&plan, "triton-fdb", fdb_img, fdb_payload(&plan, fdb_img))?;
    if !plan.dry_run {
        gate_fdb(fdb_uuid).context("FDB never reached a configured/healthy state")?;
    }

    // ── tritond + admin-backend (required, after FDB) ─────────────────
    let trd_img = require_image(&manifest.images, "triton-tritond")?;
    let trd_uuid = ensure_zone(
        &plan,
        "triton-tritond",
        trd_img,
        tritond_payload(&plan, trd_img),
    )?;
    if !plan.dry_run {
        gate_http(
            &format!("http://{}:8080/v1/health", plan.tritond_ip),
            "tritond",
        )
        .context("tritond never became healthy")?;
    }

    // ── mantad (optional, S3; FDB-independent) ────────────────────────
    let mut mantad_uuid = None;
    if plan.with_mantad {
        match manifest.images.get("triton-mantad") {
            Some(img) => {
                let ip = plan
                    .mantad_ip
                    .clone()
                    .context("--with-mantad needs --mantad-ip")?;
                let u = ensure_zone(&plan, "triton-mantad", img, mantad_payload(&plan, img))?;
                if !plan.dry_run {
                    gate_http(&format!("http://{ip}:7443/"), "mantad").ok();
                }
                mantad_uuid = Some(u);
            }
            None => warn!("triton-mantad not in channel; skipping (publish it then re-run)"),
        }
    }

    // ── clickhouse (optional, metrics; FDB-independent) ───────────────
    if plan.with_clickhouse {
        match manifest.images.get("triton-clickhouse") {
            Some(img) => {
                let ip = plan
                    .clickhouse_ip
                    .clone()
                    .context("--with-clickhouse needs --clickhouse-ip")?;
                ensure_zone(
                    &plan,
                    "triton-clickhouse",
                    img,
                    clickhouse_payload(&plan, img),
                )?;
                if !plan.dry_run {
                    gate_http(&format!("http://{ip}:8123/ping"), "clickhouse").ok();
                }
            }
            None => warn!("triton-clickhouse not in channel; skipping"),
        }
    }

    // ── GZ agents ─────────────────────────────────────────────────────
    if !plan.dry_run {
        if let Some(agent) = manifest.agents.get("tritonagent") {
            crate::install::install_agent("tritonagent", agent)?;
            write_agent_env(&plan.tritond_ip)?;
            svcadm("restart", "site/tritonagent").ok();
        } else {
            warn!("tritonagent not in channel; CNs won't have an agent");
        }
        if let Some(agent) = manifest.agents.get("proteusadm") {
            crate::install::install_agent("proteusadm", agent)?;
        }
    }

    if plan.dry_run {
        println!("\n== dry run complete — nothing was changed ==");
        return Ok(());
    }

    print_summary(&plan, trd_uuid, mantad_uuid);
    Ok(())
}

// ── plan resolution ───────────────────────────────────────────────────

fn resolve_plan(opts: SetupOpts) -> Result<Plan> {
    let cfg = match &opts.config {
        Some(p) => {
            let raw = fs::read_to_string(p).with_context(|| format!("reading config {p}"))?;
            toml::from_str::<SetupConfig>(&raw).with_context(|| format!("parsing {p}"))?
        }
        None => SetupConfig::default(),
    };

    // flags win over config.
    let fdb_ip = opts
        .fdb_ip
        .or(cfg.fdb_ip)
        .context("fdb_ip is required (--fdb-ip or config)")?;
    let tritond_ip = opts
        .tritond_ip
        .or(cfg.tritond_ip)
        .context("tritond_ip is required (--tritond-ip or config)")?;
    let with_mantad = opts.with_mantad || cfg.with_mantad.unwrap_or(false);
    let with_clickhouse = opts.with_clickhouse || cfg.with_clickhouse.unwrap_or(false);

    // Mint the shared FDB cluster secret once (reused by fdb + tritond).
    let fdb_secret = match opts.fdb_secret {
        Some(s) if !s.is_empty() && s != "generate" => s,
        _ => hex::encode(rand::random::<u128>().to_le_bytes()),
    };

    Ok(Plan {
        nic_tag: opts
            .nic_tag
            .or(cfg.nic_tag)
            .unwrap_or_else(|| DEFAULT_NIC_TAG.to_string()),
        netmask: opts
            .netmask
            .or(cfg.netmask)
            .unwrap_or_else(|| DEFAULT_NETMASK.to_string()),
        gateway: opts.gateway.or(cfg.gateway),
        resolver: opts.resolver.or(cfg.resolver),
        mantad_ip: opts.mantad_ip.or(cfg.mantad_ip),
        clickhouse_ip: opts.clickhouse_ip.or(cfg.clickhouse_ip),
        channel_url: opts
            .channel_url
            .or(cfg.channel_url)
            .unwrap_or_else(crate::install::default_channel_url),
        fdb_ip,
        tritond_ip,
        fdb_secret,
        with_mantad,
        with_clickhouse,
        dry_run: opts.dry_run,
    })
}

fn require_image<'a>(
    images: &'a std::collections::BTreeMap<String, ImageEntry>,
    name: &str,
) -> Result<&'a ImageEntry> {
    images.get(name).ok_or_else(|| {
        anyhow!("required image `{name}` is not in the channel — publish it first (`tcadm install --list`)")
    })
}

// ── per-zone vmadm payloads (faithful to images/triton-*/README.md) ───

/// Common joyent-minimal control-plane zone skeleton with one admin NIC
/// on a static IP. Callers add `customer_metadata`.
fn base_payload(
    plan: &Plan,
    alias: &str,
    image_uuid: Uuid,
    ip: &str,
    ram_gb: u64,
    quota_gb: u64,
) -> Value {
    let mut nic = json!({
        "interface": "net0",
        "nic_tag": plan.nic_tag,
        "ip": ip,
        "netmask": plan.netmask,
        "vlan_id": 0,
        "mtu": 1500,
        "primary": true,
    });
    if let Some(gw) = &plan.gateway {
        nic["gateway"] = json!(gw);
    }
    let ram = ram_gb * 1024;
    let mut payload = json!({
        "uuid": Uuid::new_v4(),
        "brand": "joyent-minimal",
        "image_uuid": image_uuid,
        "alias": alias,
        "hostname": alias,
        "delegate_dataset": true,
        "ram": ram,
        "max_physical_memory": ram,
        "cpu_cap": 200,
        "quota": quota_gb,
        "nics": [nic],
    });
    if let Some(r) = &plan.resolver {
        payload["resolvers"] = json!([r]);
    }
    payload
}

fn fdb_payload(plan: &Plan, img: &ImageEntry) -> Value {
    let mut p = base_payload(plan, "triton-fdb", img.uuid, &plan.fdb_ip, 4, 50);
    p["customer_metadata"] = json!({
        "triton:fdb_public_ip": plan.fdb_ip,
        "triton:fdb_cluster_secret": plan.fdb_secret,
        "triton:fdb_cluster_peers": plan.fdb_ip,
        "user-script": "#!/bin/sh\nsvccfg import /opt/triton/fdb/smf/triton-fdb.xml\nsvcadm enable -s site/triton-fdb\n",
    });
    p
}

fn tritond_payload(plan: &Plan, img: &ImageEntry) -> Value {
    let mut p = base_payload(plan, "triton-tritond", img.uuid, &plan.tritond_ip, 4, 50);
    p["customer_metadata"] = json!({
        "triton:fdb_cluster_secret": plan.fdb_secret,
        "triton:fdb_cluster_peers": plan.fdb_ip,
        "triton:tritond_bind_address": "0.0.0.0:8080",
        "triton:tritond_log_filter": "info",
        "triton:admin_bind_address": "0.0.0.0:8081",
        "triton:tritond_url": "http://127.0.0.1:8080",
        // The image README only enables triton-tritond; the zone also
        // ships admin-backend (the admin UI) shipped-disabled, so enable
        // BOTH here or the UI never comes up.
        "user-script": "#!/bin/sh\nset -e\nsvccfg import /opt/triton/tritond/smf/triton-tritond.xml\nsvcadm enable -s site/triton-tritond\nsvccfg import /opt/triton/admin-backend/smf/admin-backend.xml\nsvcadm enable -s site/admin-backend\n",
    });
    p
}

fn mantad_payload(plan: &Plan, img: &ImageEntry) -> Value {
    let ip = plan.mantad_ip.as_deref().unwrap_or(&plan.tritond_ip);
    let mut p = base_payload(plan, "triton-mantad", img.uuid, ip, 4, 100);
    // All mantad metadata is optional (the first-boot method auto-mints
    // SigV4 root creds + admin token into /data/etc/mantad/secrets.env);
    // we only pin the endpoint + region for clean S3 responses and read
    // the minted creds back afterward for the summary.
    p["customer_metadata"] = json!({
        "triton:mantad_endpoint_url": format!("http://{ip}:7443"),
        "triton:mantad_region": "us-east-1",
        "user-script": "#!/bin/sh\nset -e\nsvccfg import /opt/triton/mantad/smf/triton-mantad.xml\nsvcadm enable -s site/triton-mantad\n",
    });
    p
}

fn clickhouse_payload(plan: &Plan, img: &ImageEntry) -> Value {
    let ip = plan.clickhouse_ip.as_deref().unwrap_or(&plan.tritond_ip);
    let mut p = base_payload(plan, "triton-clickhouse", img.uuid, ip, 8, 100);
    p["customer_metadata"] = json!({
        "triton:clickhouse_http_port": "8123",
        "triton:clickhouse_tcp_port": "9000",
        "triton:clickhouse_listen": "0.0.0.0",
        "user-script": "#!/bin/sh\nset -e\nsvccfg import /opt/triton/clickhouse/smf/triton-clickhouse.xml\nsvcadm enable -s site/triton-clickhouse\n",
    });
    p
}

// ── zone provisioning ─────────────────────────────────────────────────

/// Install the image, then `vmadm create` the zone unless one with this
/// alias already exists. Returns the zone uuid.
fn ensure_zone(plan: &Plan, alias: &str, img: &ImageEntry, payload: Value) -> Result<Uuid> {
    if plan.dry_run {
        println!(
            "\n--- would install image {} (uuid {}) ---",
            alias, img.uuid
        );
        println!("--- would vmadm create {alias}:");
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(Uuid::nil());
    }

    crate::install::install_image(alias, img)?;

    if let Some(existing) = vmadm_lookup_alias(alias)? {
        println!("zone {alias} already exists ({existing}); skipping create");
        return Ok(existing);
    }

    let uuid = payload
        .get("uuid")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| anyhow!("internal: payload missing uuid"))?;
    println!("vmadm create {alias} ({uuid})");
    vmadm_create(&payload)?;
    Ok(uuid)
}

fn vmadm_create(payload: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(payload).context("serialise vmadm payload")?;
    let mut child = Command::new("vmadm")
        .arg("create")
        .stdin(Stdio::piped())
        .spawn()
        .context("spawn vmadm — is it on PATH (run in the GZ)?")?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("vmadm child had no stdin"))?
        .write_all(&bytes)
        .context("write payload to vmadm stdin")?;
    let status = child.wait().context("await vmadm create")?;
    if !status.success() {
        bail!("vmadm create exited {status}");
    }
    Ok(())
}

/// `vmadm lookup -H -o uuid alias=<alias>` → the first uuid, or None.
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

// ── health gates ──────────────────────────────────────────────────────

/// Block until the fdb zone has written `/data/version` (it does that
/// only after `fdbcli configure new single ssd` succeeds on fresh boot).
fn gate_fdb(zone: Uuid) -> Result<()> {
    print!("waiting for FDB to configure");
    let deadline = Instant::now() + FDB_GATE;
    loop {
        if let Ok(out) = Command::new("zlogin")
            .arg(zone.to_string())
            .args(["cat", "/data/version"])
            .output()
            && out.status.success()
            && String::from_utf8_lossy(&out.stdout).trim() == "730"
        {
            println!(" ok");
            return Ok(());
        }
        if Instant::now() >= deadline {
            println!();
            bail!("timed out waiting for /data/version=730 in the fdb zone");
        }
        print!(".");
        let _ = std::io::stdout().flush();
        sleep(POLL_INTERVAL);
    }
}

/// Block until an HTTP GET on `url` returns a success status.
fn gate_http(url: &str, what: &str) -> Result<()> {
    print!("waiting for {what} ({url})");
    let client = crate::http::blocking_client()?;
    let deadline = Instant::now() + HTTP_GATE;
    loop {
        if let Ok(resp) = client.get(url).send()
            && resp.status().is_success()
        {
            println!(" ok");
            return Ok(());
        }
        if Instant::now() >= deadline {
            println!();
            bail!("timed out waiting for {what} at {url}");
        }
        print!(".");
        let _ = std::io::stdout().flush();
        sleep(POLL_INTERVAL);
    }
}

// ── agent + status ────────────────────────────────────────────────────

/// Write `/opt/triton/tritonagent/etc/agent.env` so the agent SMF method
/// (which exits 1 without `TRITONAGENT_ENDPOINT`) can start.
fn write_agent_env(tritond_ip: &str) -> Result<()> {
    let dir = Path::new("/opt/triton/tritonagent/etc");
    fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let path = dir.join("agent.env");
    let body = format!("TRITONAGENT_ENDPOINT=http://{tritond_ip}:8080\n");
    fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    let mut perms = fs::metadata(&path)?.permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&path, perms)?;
    info!(endpoint = %tritond_ip, "wrote tritonagent agent.env");
    Ok(())
}

fn svcadm(verb: &str, fmri: &str) -> Result<()> {
    let status = Command::new("svcadm").arg(verb).arg(fmri).status()?;
    if !status.success() {
        bail!("svcadm {verb} {fmri} exited {status}");
    }
    Ok(())
}

/// Read a marker'd line out of a zone file via zlogin (best-effort).
fn zlogin_grep(zone: Uuid, marker: &str, file: &str) -> Option<String> {
    let out = Command::new("zlogin")
        .arg(zone.to_string())
        .args(["grep", "-A1", marker, file])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn print_summary(plan: &Plan, tritond_zone: Uuid, mantad_zone: Option<Uuid>) {
    println!("\n========================================================");
    println!(" headnode is up.");
    println!("  tritond API : http://{}:8080", plan.tritond_ip);
    println!("  admin UI    : http://{}:8081", plan.tritond_ip);
    match zlogin_grep(
        tritond_zone,
        "WRITE THIS DOWN",
        "/data/state/tritond/server.out",
    ) {
        Some(pw) => println!("  root password: {pw}   (shown once — store it)"),
        None => println!(
            "  root password: read it from the zone:\n    zlogin {tritond_zone} grep -A1 'WRITE THIS DOWN' /data/state/tritond/server.out"
        ),
    }
    if let (Some(z), Some(ip)) = (mantad_zone, plan.mantad_ip.as_deref()) {
        println!("  mantad (S3) : http://{ip}:7443");
        println!("    creds      : zlogin {z} cat /data/etc/mantad/secrets.env");
    }
    println!("\n next steps:");
    println!(
        "  tcadm configure --endpoint {}:8080   # then log in as root",
        plan.tritond_ip
    );
    println!("  tcadm api-key create                  # stop reusing the root password");
    if plan.with_mantad {
        println!("  tcadm storage cluster add ...         # register mantad with tritond");
    }
    if plan.with_clickhouse {
        println!(
            "  tcadm config set metrics.backend clickhouse && tcadm config set metrics.clickhouse_url http://{}:8123",
            plan.clickhouse_ip.as_deref().unwrap_or("<ch-ip>")
        );
    }
    println!("  # then on each CN: tcadm install tritonagent + approve via `tcadm cn approve`");
    println!("========================================================");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_plan() -> Plan {
        Plan {
            nic_tag: "admin".into(),
            netmask: "255.255.255.0".into(),
            gateway: Some("10.0.0.1".into()),
            resolver: Some("8.8.8.8".into()),
            fdb_ip: "10.0.0.10".into(),
            tritond_ip: "10.0.0.11".into(),
            mantad_ip: Some("10.0.0.12".into()),
            clickhouse_ip: Some("10.0.0.13".into()),
            fdb_secret: "deadbeefdeadbeef".into(),
            channel_url: "https://x/stable.json".into(),
            with_mantad: true,
            with_clickhouse: true,
            dry_run: false,
        }
    }

    fn img() -> ImageEntry {
        serde_json::from_value(json!({
            "stamp": "20260101T000000Z",
            "uuid": "11111111-1111-1111-1111-111111111111",
            "manifest_url": "https://x/m.json",
            "content_url": "https://x/c.zfs.gz",
            "sha256": "00",
            "size_bytes": 1,
            "data_format_version": 730,
            "data_format_min_read": 730,
        }))
        .unwrap()
    }

    fn meta_keys(p: &Value) -> Vec<String> {
        p["customer_metadata"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn base_shape_is_joyent_minimal_single_admin_nic() {
        let p = base_payload(&test_plan(), "z", img().uuid, "10.0.0.10", 4, 50);
        assert_eq!(p["brand"], "joyent-minimal");
        assert_eq!(p["delegate_dataset"], true);
        assert_eq!(p["ram"], 4096);
        assert_eq!(p["max_physical_memory"], 4096);
        let nics = p["nics"].as_array().unwrap();
        assert_eq!(nics.len(), 1);
        assert_eq!(nics[0]["nic_tag"], "admin");
        assert_eq!(nics[0]["ip"], "10.0.0.10");
        assert_eq!(nics[0]["primary"], true);
        assert_eq!(nics[0]["gateway"], "10.0.0.1");
    }

    #[test]
    fn fdb_payload_carries_exact_first_boot_keys() {
        let p = fdb_payload(&test_plan(), &img());
        let keys = meta_keys(&p);
        for k in [
            "triton:fdb_public_ip",
            "triton:fdb_cluster_secret",
            "triton:fdb_cluster_peers",
            "user-script",
        ] {
            assert!(keys.contains(&k.to_string()), "missing {k}: {keys:?}");
        }
        // public_ip MUST equal the NIC ip (the method exits 1 otherwise).
        assert_eq!(
            p["customer_metadata"]["triton:fdb_public_ip"],
            p["nics"][0]["ip"]
        );
        assert!(
            p["customer_metadata"]["user-script"]
                .as_str()
                .unwrap()
                .contains("site/triton-fdb")
        );
    }

    #[test]
    fn tritond_requires_fdb_creds_and_enables_both_services() {
        let p = tritond_payload(&test_plan(), &img());
        let keys = meta_keys(&p);
        for k in ["triton:fdb_cluster_secret", "triton:fdb_cluster_peers"] {
            assert!(keys.contains(&k.to_string()), "missing {k}");
        }
        let us = p["customer_metadata"]["user-script"].as_str().unwrap();
        // adminui must come up too.
        assert!(us.contains("site/triton-tritond"));
        assert!(
            us.contains("site/admin-backend"),
            "adminui not enabled: {us}"
        );
    }

    #[test]
    fn fdb_and_tritond_share_the_same_secret() {
        let plan = test_plan();
        let f = fdb_payload(&plan, &img());
        let t = tritond_payload(&plan, &img());
        assert_eq!(
            f["customer_metadata"]["triton:fdb_cluster_secret"],
            t["customer_metadata"]["triton:fdb_cluster_secret"]
        );
        assert_eq!(
            f["customer_metadata"]["triton:fdb_cluster_secret"],
            json!("deadbeefdeadbeef")
        );
    }

    #[test]
    fn mantad_and_clickhouse_user_scripts_target_right_services() {
        let plan = test_plan();
        let m = mantad_payload(&plan, &img());
        assert!(
            m["customer_metadata"]["user-script"]
                .as_str()
                .unwrap()
                .contains("site/triton-mantad")
        );
        let c = clickhouse_payload(&plan, &img());
        assert!(
            c["customer_metadata"]["user-script"]
                .as_str()
                .unwrap()
                .contains("site/triton-clickhouse")
        );
    }

    #[test]
    fn minted_secret_is_hex_and_nonempty() {
        let opts = SetupOpts {
            config: None,
            fdb_ip: Some("10.0.0.10".into()),
            tritond_ip: Some("10.0.0.11".into()),
            mantad_ip: None,
            clickhouse_ip: None,
            netmask: None,
            gateway: None,
            resolver: None,
            nic_tag: None,
            channel_url: Some("https://x/s.json".into()),
            with_mantad: false,
            with_clickhouse: false,
            fdb_secret: None,
            dry_run: true,
        };
        let plan = resolve_plan(opts).unwrap();
        assert_eq!(plan.fdb_secret.len(), 32);
        assert!(plan.fdb_secret.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
