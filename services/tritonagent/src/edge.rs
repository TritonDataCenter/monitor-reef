// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Host-side executor for firehyve/fhrun edge instances.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
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
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(5);
const TERMINATE_POLL: Duration = Duration::from_millis(50);

/// Apply one edge instance manifest by persisting it under
/// `edge_root/<edge_instance_id>` and supervising a local fhrun process.
pub fn apply(
    edge_root: &Path,
    fhrun_bin: &Path,
    edge_instance_id: Uuid,
    manifest_bytes: &[u8],
) -> Result<()> {
    let runtime_dir = runtime_dir(edge_root, edge_instance_id);
    fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("create edge runtime dir {}", runtime_dir.display()))?;

    let manifest: Manifest = serde_json::from_slice(manifest_bytes)
        .with_context(|| format!("parse edge manifest for {edge_instance_id}"))?;
    validate_manifest_contract(&manifest, &runtime_dir)?;

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
            return Ok(());
        }
    }

    write_manifest_atomically(&runtime_dir, manifest_bytes)?;
    check_fhrun(fhrun_bin, &manifest_path)?;

    if let Some(pid) = old_pid {
        terminate_pid(pid)
            .with_context(|| format!("stop prior fhrun pid {pid} for edge {edge_instance_id}"))?;
    }

    let pid = spawn_fhrun(fhrun_bin, &manifest_path, &runtime_dir)?;
    fs::write(&pid_path, format!("{pid}\n"))
        .with_context(|| format!("write edge fhrun pid {}", pid_path.display()))?;
    info!(
        edge_instance_id = %edge_instance_id,
        pid,
        manifest = %manifest_path.display(),
        "started edge fhrun process",
    );
    Ok(())
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
    use std::os::unix::fs::PermissionsExt;

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
    #[test]
    fn apply_writes_manifest_checks_fhrun_and_starts_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("edge");
        let fhrun = fake_fhrun(temp.path());
        let edge_instance_id = Uuid::new_v4();
        let manifest = serde_json::to_vec(&manifest(
            edge_instance_id,
            &root,
            DATAPLANE_BACKEND_NFTABLES,
        ))
        .expect("manifest json");

        apply(&root, &fhrun, edge_instance_id, &manifest).expect("apply edge");

        let runtime_dir = root.join(edge_instance_id.to_string());
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

        apply(&root, &fhrun, edge_instance_id, &manifest).expect("idempotent apply");
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
}
