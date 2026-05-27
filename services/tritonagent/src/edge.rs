// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Legacy host-side executor for firehyve/fhrun edge instances.
//!
//! This module is a pre-vmadm compatibility shim. The M1 edge runtime
//! target is a SmartOS zone whose lifecycle is owned by `vmadm`, with
//! fhrun/firehyve running inside the zone after tritonagent creates the
//! required north/south links.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use edge_manifest::{
    DATAPLANE_BACKEND_AFXDP, DATAPLANE_BACKEND_NFTABLES, EDGE_CONTROL_PROTOCOL_V1, Manifest,
};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

const MANIFEST_FILE: &str = "manifest.json";
const MANIFEST_TMP_FILE: &str = "manifest.json.tmp";
const PID_FILE: &str = "fhrun.pid";
const STDOUT_LOG: &str = "fhrun.stdout.log";
const STDERR_LOG: &str = "fhrun.stderr.log";
const CONTROL_SOCKET_FILE: &str = "edge-control.sock";
const CONTROL_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_POLL: Duration = Duration::from_millis(250);
const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(5);
const TERMINATE_POLL: Duration = Duration::from_millis(50);

/// Health observed from the in-guest edge-agent after fhrun starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeApplyStatus {
    pub backend: String,
    pub healthy: bool,
    pub last_ruleset_bytes: u64,
    pub error: Option<String>,
}

pub fn apply(
    edge_root: &Path,
    fhrun_bin: &Path,
    edge_instance_id: Uuid,
    manifest_bytes: &[u8],
) -> Result<EdgeApplyStatus> {
    let runtime_dir = runtime_dir(edge_root, edge_instance_id);
    fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("create edge runtime dir {}", runtime_dir.display()))?;

    let manifest: Manifest = serde_json::from_slice(manifest_bytes)
        .with_context(|| format!("parse edge manifest for {edge_instance_id}"))?;
    validate_manifest_contract(&manifest, &runtime_dir)?;
    let control_socket = manifest
        .edge_control_socket_path(&runtime_dir)
        .ok_or_else(|| anyhow::anyhow!("edge manifest must enable edge_control for v1"))?;

    let manifest_path = runtime_dir.join(MANIFEST_FILE);
    let pid_path = runtime_dir.join(PID_FILE);
    let old_pid = read_pid(&pid_path)?;
    let manifest_changed = manifest_changed(&manifest_path, manifest_bytes)?;

    if let Some(pid) = old_pid {
        if process_is_running(pid) && !manifest_changed {
            info!(
                edge_instance_id = %edge_instance_id,
                pid,
                manifest = %manifest_path.display(),
                "edge instance already running with desired manifest",
            );
            return probe_edge_control(&control_socket, CONTROL_CONNECT_TIMEOUT).with_context(
                || format!("probe edge control socket {}", control_socket.display()),
            );
        }
    }

    write_manifest_atomically(&runtime_dir, manifest_bytes)?;
    check_fhrun(fhrun_bin, &manifest_path)?;

    if let Some(pid) = old_pid {
        terminate_pid(pid)
            .with_context(|| format!("stop prior fhrun pid {pid} for edge {edge_instance_id}"))?;
    }

    remove_stale_control_socket(&control_socket)?;
    let pid = spawn_fhrun(fhrun_bin, &manifest_path, &runtime_dir)?;
    fs::write(&pid_path, format!("{pid}\n"))
        .with_context(|| format!("write edge fhrun pid {}", pid_path.display()))?;
    info!(
        edge_instance_id = %edge_instance_id,
        pid,
        manifest = %manifest_path.display(),
        "started edge fhrun process",
    );
    match probe_edge_control(&control_socket, CONTROL_CONNECT_TIMEOUT)
        .with_context(|| format!("probe edge control socket {}", control_socket.display()))
    {
        Ok(status) => Ok(status),
        Err(err) => {
            if let Err(stop_err) = terminate_pid(pid) {
                warn!(
                    pid,
                    error = %stop_err,
                    "failed to stop fhrun after edge control probe failure",
                );
            }
            Err(err)
        }
    }
}

/// Stop and remove one edge instance runtime directory. Missing or
/// already-dead runners are treated as success so reap is idempotent.
pub fn reap(edge_root: &Path, edge_instance_id: Uuid) -> Result<()> {
    let runtime_dir = runtime_dir(edge_root, edge_instance_id);
    let pid_path = runtime_dir.join(PID_FILE);
    if let Some(pid) = read_pid(&pid_path)? {
        if process_is_running(pid) {
            terminate_pid(pid)
                .with_context(|| format!("stop fhrun pid {pid} for edge {edge_instance_id}"))?;
        }
    }

    match fs::remove_dir_all(&runtime_dir) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("remove edge runtime dir {}", runtime_dir.display()));
        }
    }
    info!(
        edge_instance_id = %edge_instance_id,
        runtime_dir = %runtime_dir.display(),
        "reaped edge runtime",
    );
    Ok(())
}

fn runtime_dir(edge_root: &Path, edge_instance_id: Uuid) -> PathBuf {
    edge_root.join(edge_instance_id.to_string())
}

fn validate_manifest_contract(manifest: &Manifest, runtime_dir: &Path) -> Result<()> {
    if manifest.name.is_empty() || manifest.name.len() > 127 {
        bail!("manifest.name must be 1..=127 chars");
    }
    if manifest.name.as_bytes().contains(&0) {
        bail!("manifest.name must not contain NUL");
    }
    if manifest.vcpus == 0 {
        bail!("manifest.vcpus must be >= 1");
    }
    let nic_count = manifest.all_nics().count();
    if nic_count > 4 {
        bail!("too many NICs: max 4, got {nic_count}");
    }
    for nic in manifest.all_nics() {
        if nic.vnic.is_empty() {
            bail!("nic.vnic must not be empty");
        }
        if nic.mac.split(':').count() != 6 {
            bail!("nic.mac must be xx:xx:xx:xx:xx:xx (got {})", nic.mac);
        }
        if !nic.ip.contains('/') {
            bail!("nic.ip must be CIDR form (got {})", nic.ip);
        }
    }

    if let Some(dataplane) = manifest.dataplane.as_ref() {
        match dataplane.backend.as_str() {
            DATAPLANE_BACKEND_NFTABLES => {}
            DATAPLANE_BACKEND_AFXDP => {
                bail!(
                    "manifest.dataplane.backend \"afxdp\" is reserved for a future backend; v1 supports \"nftables\""
                );
            }
            other => bail!("manifest.dataplane.backend must be \"nftables\" for v1 (got {other})"),
        }
    }

    if let Some(control) = manifest.edge_control.as_ref() {
        if control.guest_device.is_empty() {
            bail!("edge_control.guest_device must not be empty");
        }
        if control.protocol != EDGE_CONTROL_PROTOCOL_V1 {
            bail!(
                "edge_control.protocol must be {} for v1",
                EDGE_CONTROL_PROTOCOL_V1
            );
        }
        if control
            .socket
            .as_ref()
            .is_some_and(|socket| socket.as_os_str().is_empty())
        {
            bail!("edge_control.socket must not be empty");
        }
    }

    let expected_socket = runtime_dir.join(CONTROL_SOCKET_FILE);
    if let Some(socket) = manifest.edge_control_socket_path(runtime_dir) {
        if socket != expected_socket {
            bail!(
                "edge_control.socket must be {} for this edge instance (got {})",
                expected_socket.display(),
                socket.display(),
            );
        }
    }
    Ok(())
}

fn manifest_changed(path: &Path, manifest_bytes: &[u8]) -> Result<bool> {
    match fs::read(path) {
        Ok(current) => Ok(sha256(&current) != sha256(manifest_bytes)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(true),
        Err(err) => Err(err).with_context(|| format!("read existing manifest {}", path.display())),
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn write_manifest_atomically(runtime_dir: &Path, manifest_bytes: &[u8]) -> Result<()> {
    let tmp = runtime_dir.join(MANIFEST_TMP_FILE);
    let final_path = runtime_dir.join(MANIFEST_FILE);
    fs::write(&tmp, manifest_bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &final_path)
        .with_context(|| format!("rename {} to {}", tmp.display(), final_path.display()))?;
    Ok(())
}

fn remove_stale_control_socket(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => {
            Err(err).with_context(|| format!("remove stale edge socket {}", path.display()))
        }
    }
}

#[cfg(unix)]
fn probe_edge_control(socket: &Path, timeout: Duration) -> Result<EdgeApplyStatus> {
    let deadline = Instant::now() + timeout;
    let mut last_wait = format!("edge control socket {} is not ready", socket.display());
    loop {
        match probe_edge_control_once(socket)? {
            ProbeAttempt::Ready(status) => return Ok(status),
            ProbeAttempt::NotReady(message) => last_wait = message,
        }

        if Instant::now() >= deadline {
            bail!(
                "edge control socket {} did not become healthy within {:?}: {}",
                socket.display(),
                timeout,
                last_wait
            );
        }
        thread::sleep(CONTROL_POLL);
    }
}

#[cfg(not(unix))]
fn probe_edge_control(_socket: &Path, _timeout: Duration) -> Result<EdgeApplyStatus> {
    bail!("edge control sockets require Unix domain socket support")
}

#[cfg(unix)]
enum ProbeAttempt {
    Ready(EdgeApplyStatus),
    NotReady(String),
}

#[cfg(unix)]
fn probe_edge_control_once(socket: &Path) -> Result<ProbeAttempt> {
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::TimedOut
            ) =>
        {
            return Ok(ProbeAttempt::NotReady(format!(
                "connect {}: {}",
                socket.display(),
                err
            )));
        }
        Err(err) => {
            return Err(err).with_context(|| format!("connect {}", socket.display()));
        }
    };
    stream
        .set_read_timeout(Some(CONTROL_REQUEST_TIMEOUT))
        .with_context(|| format!("set read timeout on {}", socket.display()))?;
    stream
        .set_write_timeout(Some(CONTROL_REQUEST_TIMEOUT))
        .with_context(|| format!("set write timeout on {}", socket.display()))?;
    let reader_stream = stream
        .try_clone()
        .with_context(|| format!("clone edge control stream {}", socket.display()))?;
    let mut reader = BufReader::new(reader_stream);

    let ping = send_control_request(&mut stream, &mut reader, "ping", "ping")
        .with_context(|| format!("edge control ping {}", socket.display()))?;
    validate_control_protocol(&ping, "ping")?;

    let result = send_control_request(&mut stream, &mut reader, "status", "status")
        .with_context(|| format!("edge control status {}", socket.display()))?;
    let status = parse_control_status(result)?;
    if status.healthy {
        Ok(ProbeAttempt::Ready(status))
    } else {
        Ok(ProbeAttempt::NotReady(format!(
            "edge-agent backend {} unhealthy: {}",
            status.backend,
            status.error.as_deref().unwrap_or("no diagnostic")
        )))
    }
}

#[cfg(unix)]
fn send_control_request(
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    id: &str,
    method: &str,
) -> Result<serde_json::Value> {
    serde_json::to_writer(
        &mut *stream,
        &serde_json::json!({
            "id": id,
            "method": method,
        }),
    )
    .with_context(|| format!("encode edge control {method} request"))?;
    stream
        .write_all(b"\n")
        .with_context(|| format!("write edge control {method} request"))?;
    stream
        .flush()
        .with_context(|| format!("flush edge control {method} request"))?;

    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .with_context(|| format!("read edge control {method} response"))?;
    if read == 0 {
        bail!("edge control closed before {method} response");
    }
    let response: serde_json::Value = serde_json::from_str(&line)
        .with_context(|| format!("parse edge control {method} response"))?;
    if response.get("id").and_then(serde_json::Value::as_str) != Some(id) {
        bail!("edge control {method} response id mismatch");
    }
    if !response
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let message = response
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("no diagnostic");
        bail!("edge control {method} failed: {message}");
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("edge control {method} response missing result"))
}

fn validate_control_protocol(result: &serde_json::Value, method: &str) -> Result<()> {
    let protocol = result
        .get("protocol")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("edge control {method} result missing protocol"))?;
    if protocol != EDGE_CONTROL_PROTOCOL_V1 {
        bail!("edge control {method} protocol must be {EDGE_CONTROL_PROTOCOL_V1} (got {protocol})");
    }
    Ok(())
}

fn parse_control_status(result: serde_json::Value) -> Result<EdgeApplyStatus> {
    validate_control_protocol(&result, "status")?;
    let backend = result
        .get("backend")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("edge control status result missing backend"))?;
    match backend {
        DATAPLANE_BACKEND_NFTABLES => {}
        DATAPLANE_BACKEND_AFXDP => {
            bail!("edge control status reported future backend \"afxdp\"; v1 expects \"nftables\"");
        }
        other => bail!("edge control status reported unsupported backend {other}"),
    }
    let healthy = result
        .get("healthy")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("edge control status result missing healthy"))?;
    let last_ruleset_bytes = result
        .get("last_ruleset_bytes")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let error = result
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    Ok(EdgeApplyStatus {
        backend: backend.to_string(),
        healthy,
        last_ruleset_bytes,
        error,
    })
}

fn check_fhrun(fhrun_bin: &Path, manifest_path: &Path) -> Result<()> {
    let output = Command::new(fhrun_bin)
        .arg("--check")
        .arg(manifest_path)
        .output()
        .with_context(|| format!("run {} --check", fhrun_bin.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    bail!(
        "fhrun manifest check failed with status {}: {}{}{}",
        output.status,
        stdout.trim(),
        if stdout.is_empty() || stderr.is_empty() {
            ""
        } else {
            "; "
        },
        stderr.trim(),
    );
}

fn spawn_fhrun(fhrun_bin: &Path, manifest_path: &Path, runtime_dir: &Path) -> Result<i32> {
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(runtime_dir.join(STDOUT_LOG))
        .with_context(|| format!("open edge stdout log in {}", runtime_dir.display()))?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(runtime_dir.join(STDERR_LOG))
        .with_context(|| format!("open edge stderr log in {}", runtime_dir.display()))?;

    let mut child = Command::new(fhrun_bin)
        .arg(manifest_path)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .with_context(|| format!("spawn fhrun {}", fhrun_bin.display()))?;
    let pid = i32::try_from(child.id()).context("fhrun child pid does not fit pid_t")?;
    let manifest_path = manifest_path.to_path_buf();
    thread::Builder::new()
        .name(format!("edge-fhrun-wait-{pid}"))
        .spawn(move || match child.wait() {
            Ok(status) => {
                info!(
                    pid,
                    status = %status,
                    manifest = %manifest_path.display(),
                    "edge fhrun process exited",
                );
            }
            Err(err) => {
                warn!(
                    pid,
                    error = %err,
                    manifest = %manifest_path.display(),
                    "failed to wait for edge fhrun process",
                );
            }
        })
        .context("spawn edge fhrun wait thread")?;
    Ok(pid)
}

fn read_pid(path: &Path) -> Result<Option<i32>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let pid = trimmed
        .parse::<i32>()
        .with_context(|| format!("parse pid from {}", path.display()))?;
    Ok(Some(pid))
}

fn process_is_running(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) == 0 } {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn terminate_pid(pid: i32) -> Result<()> {
    if pid <= 0 || !process_is_running(pid) {
        return Ok(());
    }
    signal_pid(pid, libc::SIGTERM)?;
    let deadline = Instant::now() + TERMINATE_TIMEOUT;
    while Instant::now() < deadline {
        if !process_is_running(pid) {
            return Ok(());
        }
        thread::sleep(TERMINATE_POLL);
    }
    signal_pid(pid, libc::SIGKILL)?;
    Ok(())
}

fn signal_pid(pid: i32, signal: i32) -> Result<()> {
    let rc = unsafe { libc::kill(pid, signal) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err).with_context(|| format!("signal pid {pid} with {signal}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use edge_manifest::{
        DataplaneConfig, EDGE_CONTROL_GUEST_DEVICE, EdgeControlConfig, NetConfig, SnatRule,
    };

    #[cfg(unix)]
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};

    fn manifest(edge_instance_id: Uuid, root: &Path, backend: &str) -> Manifest {
        let runtime_dir = root.join(edge_instance_id.to_string());
        Manifest {
            name: format!("triton-edge-{edge_instance_id}"),
            bin: PathBuf::from("/opt/firehyve/bin/edge-agent"),
            args: Vec::new(),
            env: BTreeMap::new(),
            workdir: "/".to_string(),
            vcpus: 1,
            memory: "128M".to_string(),
            kernel: PathBuf::from("/opt/firehyve/kernels/linux-v1/bzImage"),
            init: PathBuf::from("/opt/firehyve/bin/fhrun-init"),
            extra_files: BTreeMap::new(),
            net: None,
            nics: vec![
                NetConfig {
                    vnic: "edge_north0".to_string(),
                    mac: "02:00:00:00:00:10".to_string(),
                    ip: "203.0.113.10/24".to_string(),
                    gateway: Some("203.0.113.1".to_string()),
                    role: Some("north".to_string()),
                },
                NetConfig {
                    vnic: "edge_south0".to_string(),
                    mac: "02:00:00:00:00:11".to_string(),
                    ip: "fd00::40/64".to_string(),
                    gateway: None,
                    role: Some("south".to_string()),
                },
            ],
            dataplane: Some(DataplaneConfig {
                backend: backend.to_string(),
                snat: vec![SnatRule {
                    from: "10.0.1.0/24".to_string(),
                    via: "203.0.113.10".to_string(),
                }],
                fips: Vec::new(),
                load_balancers: Vec::new(),
                bgp: None,
                control_listen: None,
            }),
            edge_control: Some(EdgeControlConfig {
                socket: Some(runtime_dir.join(CONTROL_SOCKET_FILE)),
                guest_device: EDGE_CONTROL_GUEST_DEVICE.to_string(),
                protocol: EDGE_CONTROL_PROTOCOL_V1.to_string(),
            }),
            firehyve: PathBuf::from("/opt/firehyve/bin/firehyve"),
            kernel_extra_cmdline: String::new(),
        }
    }

    #[cfg(unix)]
    fn fake_fhrun(dir: &Path) -> PathBuf {
        let path = dir.join("fake-fhrun");
        fs::write(
            &path,
            r#"#!/bin/sh
echo "$@" >> "$0.log"
if [ "$1" = "--check" ]; then
  exit 0
fi
trap 'exit 0' TERM INT
while true; do
  sleep 1
done
"#,
        )
        .expect("write fake fhrun");
        let mut perms = fs::metadata(&path)
            .expect("fake fhrun metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod fake fhrun");
        path
    }

    #[cfg(unix)]
    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("tae")
            .tempdir_in("/tmp")
            .expect("short tempdir")
    }

    #[cfg(unix)]
    fn healthy_control_status(last_ruleset_bytes: u64) -> serde_json::Value {
        serde_json::json!({
            "protocol": EDGE_CONTROL_PROTOCOL_V1,
            "backend": DATAPLANE_BACKEND_NFTABLES,
            "healthy": true,
            "error": null,
            "shutting_down": false,
            "last_ruleset_bytes": last_ruleset_bytes,
        })
    }

    #[cfg(unix)]
    fn unhealthy_control_status(message: &str) -> serde_json::Value {
        serde_json::json!({
            "protocol": EDGE_CONTROL_PROTOCOL_V1,
            "backend": DATAPLANE_BACKEND_NFTABLES,
            "healthy": false,
            "error": message,
            "shutting_down": false,
            "last_ruleset_bytes": 0,
        })
    }

    #[cfg(unix)]
    fn spawn_fake_edge_control(
        socket: PathBuf,
        status: serde_json::Value,
    ) -> thread::JoinHandle<()> {
        fs::create_dir_all(socket.parent().expect("socket parent")).expect("socket parent dir");
        let _ = fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("bind fake edge control");
        thread::spawn(move || serve_one_edge_control_client(listener, status))
    }

    #[cfg(unix)]
    fn spawn_fake_edge_control_after_fhrun_start(
        socket: PathBuf,
        fhrun_log: PathBuf,
        status: serde_json::Value,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                let started = fs::read_to_string(&fhrun_log)
                    .map(|log| log.lines().any(|line| !line.contains("--check")))
                    .unwrap_or(false);
                if started {
                    let _ = fs::remove_file(&socket);
                    let listener = UnixListener::bind(&socket).expect("bind fake edge control");
                    serve_one_edge_control_client(listener, status);
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("fake fhrun did not start before edge control server deadline");
        })
    }

    #[cfg(unix)]
    fn serve_one_edge_control_client(listener: UnixListener, status: serde_json::Value) {
        let (mut stream, _) = listener.accept().expect("accept edge control client");
        let reader_stream = stream.try_clone().expect("clone edge control client");
        let mut reader = BufReader::new(reader_stream);
        for _ in 0..2 {
            let mut line = String::new();
            let read = reader
                .read_line(&mut line)
                .expect("read edge control request");
            if read == 0 {
                return;
            }
            let request: serde_json::Value =
                serde_json::from_str(&line).expect("parse edge control request");
            let id = request
                .get("id")
                .and_then(serde_json::Value::as_str)
                .expect("request id");
            let method = request
                .get("method")
                .and_then(serde_json::Value::as_str)
                .expect("request method");
            let result = match method {
                "ping" => serde_json::json!({ "protocol": EDGE_CONTROL_PROTOCOL_V1 }),
                "status" => status.clone(),
                other => panic!("unexpected edge control method {other}"),
            };
            let response = serde_json::json!({
                "id": id,
                "ok": true,
                "result": result,
            });
            serde_json::to_writer(&mut stream, &response).expect("write edge control response");
            stream
                .write_all(b"\n")
                .expect("terminate edge control response");
            stream.flush().expect("flush edge control response");
        }
    }

    #[cfg(unix)]
    #[test]
    fn apply_writes_manifest_checks_fhrun_and_starts_process() {
        let temp = short_tempdir();
        let root = temp.path().join("edge");
        let fhrun = fake_fhrun(temp.path());
        let edge_instance_id = Uuid::new_v4();
        let manifest = serde_json::to_vec(&manifest(
            edge_instance_id,
            &root,
            DATAPLANE_BACKEND_NFTABLES,
        ))
        .expect("manifest json");

        let runtime_dir = root.join(edge_instance_id.to_string());
        let control_socket = runtime_dir.join(CONTROL_SOCKET_FILE);
        let server = spawn_fake_edge_control_after_fhrun_start(
            control_socket.clone(),
            fhrun.with_extension("log"),
            healthy_control_status(321),
        );
        let status = apply(&root, &fhrun, edge_instance_id, &manifest).expect("apply edge");
        server.join().expect("fake edge control server");
        assert_eq!(status.backend, DATAPLANE_BACKEND_NFTABLES);
        assert!(status.healthy);
        assert_eq!(status.last_ruleset_bytes, 321);

        assert_eq!(
            fs::read(runtime_dir.join(MANIFEST_FILE)).expect("persisted manifest"),
            manifest
        );
        let pid = read_pid(&runtime_dir.join(PID_FILE))
            .expect("read pid")
            .expect("pid present");
        assert!(process_is_running(pid));

        let log = fs::read_to_string(fhrun.with_extension("log")).expect("fake fhrun log");
        assert!(log.contains("--check"));
        assert!(log.contains(MANIFEST_FILE));

        let server = spawn_fake_edge_control(control_socket, healthy_control_status(654));
        let status = apply(&root, &fhrun, edge_instance_id, &manifest).expect("idempotent apply");
        server.join().expect("fake edge control server");
        assert_eq!(status.last_ruleset_bytes, 654);
        let log_after_idempotent_apply =
            fs::read_to_string(fhrun.with_extension("log")).expect("fake fhrun log");
        assert_eq!(log_after_idempotent_apply, log);

        reap(&root, edge_instance_id).expect("reap edge");
        assert!(!runtime_dir.exists());
        assert!(!process_is_running(pid));
    }

    #[test]
    fn apply_rejects_afxdp_backend_for_v1() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("edge");
        let edge_instance_id = Uuid::new_v4();
        let manifest =
            serde_json::to_vec(&manifest(edge_instance_id, &root, DATAPLANE_BACKEND_AFXDP))
                .expect("manifest json");

        let err = apply(
            &root,
            Path::new("/does/not/matter"),
            edge_instance_id,
            &manifest,
        )
        .expect_err("afxdp should be rejected before fhrun");

        assert!(err.to_string().contains("reserved for a future backend"));
    }

    #[test]
    fn apply_rejects_control_socket_outside_runtime_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("edge");
        let edge_instance_id = Uuid::new_v4();
        let mut manifest = manifest(edge_instance_id, &root, DATAPLANE_BACKEND_NFTABLES);
        manifest.edge_control.as_mut().expect("edge control").socket =
            Some(temp.path().join("elsewhere.sock"));
        let manifest = serde_json::to_vec(&manifest).expect("manifest json");

        let err = apply(
            &root,
            Path::new("/does/not/matter"),
            edge_instance_id,
            &manifest,
        )
        .expect_err("bad socket should be rejected before fhrun");

        assert!(err.to_string().contains("edge_control.socket must be"));
    }

    #[cfg(unix)]
    #[test]
    fn probe_edge_control_rejects_unhealthy_status() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket = temp.path().join("edge-control.sock");
        let server = spawn_fake_edge_control(
            socket.clone(),
            unhealthy_control_status("nftables apply failed"),
        );

        let err = probe_edge_control(&socket, Duration::ZERO)
            .expect_err("unhealthy status should not apply");
        server.join().expect("fake edge control server");

        let message = err.to_string();
        assert!(message.contains("did not become healthy"));
        assert!(message.contains("nftables apply failed"));
    }
}
