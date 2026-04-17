// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `/usr/sbin/vmadm` wrapper.
//!
//! Mirrors the subset of [node-vmadm](https://github.com/TritonDataCenter/node-vmadm)
//! that cn-agent actually calls. Each method shells out to `vmadm` and
//! deserializes its JSON output.
//!
//! Callers inject the binary path so tests can supply mock scripts; on a
//! real compute node the default `/usr/sbin/vmadm` is used.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use thiserror::Error;

/// SmartOS ships `vmadm` at `/usr/sbin/vmadm`; node-vmadm hardcodes the
/// same path.
pub const DEFAULT_VMADM_BIN: &str = "/usr/sbin/vmadm";

/// Marker that [`VmadmError::LoadNotFound`] uses, matching the `restCode`
/// the legacy module assigns.
pub const VM_NOT_FOUND_REST_CODE: &str = "VmNotFound";

#[derive(Debug, Error)]
pub enum VmadmError {
    #[error("failed to spawn {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("vmadm exited with status {status}: {stderr}")]
    NonZeroExit { status: ExitStatus, stderr: String },
    #[error("VM {uuid} not found")]
    NotFound { uuid: String },
    /// The VM is already in the state the caller asked for (e.g., `vmadm
    /// start` on a running VM). Distinct from [`NonZeroExit`] so handlers
    /// can treat it as success when the caller passes `idempotent=true`.
    #[error("VM {uuid} is already {state}")]
    AlreadyInState { uuid: String, state: &'static str },
    #[error("failed to parse vmadm JSON: {source}")]
    Parse {
        #[source]
        source: serde_json::Error,
    },
}

impl VmadmError {
    /// Legacy restCode exposed on the HTTP error, only set for the
    /// `NotFound` variant.
    pub fn rest_code(&self) -> Option<&'static str> {
        match self {
            VmadmError::NotFound { .. } => Some(VM_NOT_FOUND_REST_CODE),
            _ => None,
        }
    }
}

/// Options for [`VmadmTool::load`].
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Return VMs with `do_not_inventory=true`. Defaults to false; callers
    /// treat those VMs as non-existent unless this flag is set.
    pub include_dni: bool,
    /// Retain only the named fields in the returned JSON. Mirrors the
    /// legacy `fields` filter.
    pub fields: Option<Vec<String>>,
}

/// Options for [`VmadmTool::info`].
#[derive(Debug, Clone, Default)]
pub struct InfoOptions {
    /// Optional list of info types to filter by (passed as a
    /// comma-separated argument to `vmadm info`).
    pub types: Vec<String>,
    pub include_dni: bool,
}

/// Options for [`VmadmTool::lookup`].
#[derive(Debug, Clone, Default)]
pub struct LookupOptions {
    pub include_dni: bool,
    pub fields: Option<Vec<String>>,
}

/// Options for [`VmadmTool::start`].
///
/// Maps to the legacy cdrom/disk/order/once `key=value` positional args
/// `vmadm start` accepts for KVM and bhyve boot overrides.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub include_dni: bool,
    /// CDROM devices to attach, one per element.
    pub cdrom: Vec<String>,
    pub disk: Vec<String>,
    pub order: Vec<String>,
    pub once: Vec<String>,
}

/// Options for [`VmadmTool::stop`].
#[derive(Debug, Clone, Default)]
pub struct StopOptions {
    pub include_dni: bool,
    /// Equivalent to `vmadm stop -F`: kill abruptly instead of graceful shutdown.
    pub force: bool,
    /// Seconds between SIGTERM and SIGKILL for docker containers (`-t`).
    pub timeout: Option<u32>,
}

/// Options for [`VmadmTool::reboot`].
#[derive(Debug, Clone, Default)]
pub struct RebootOptions {
    pub include_dni: bool,
    pub force: bool,
}

/// Options for [`VmadmTool::kill`].
#[derive(Debug, Clone, Default)]
pub struct KillOptions {
    pub include_dni: bool,
    /// Signal name to pass to `vmadm kill -s`. Defaults to SIGKILL when unset.
    pub signal: Option<String>,
}

/// Thin wrapper around the `vmadm` binary.
///
/// Cheap to clone; stores only the binary path.
#[derive(Debug, Clone)]
pub struct VmadmTool {
    pub vmadm_bin: PathBuf,
}

impl Default for VmadmTool {
    fn default() -> Self {
        Self {
            vmadm_bin: PathBuf::from(DEFAULT_VMADM_BIN),
        }
    }
}

impl VmadmTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        Self {
            vmadm_bin: bin.into(),
        }
    }

    /// `vmadm get <uuid>` → parsed VM JSON.
    ///
    /// Missing zones and DNI-filtered zones both return
    /// [`VmadmError::NotFound`] so callers can translate them to 404 /
    /// `VmNotFound` without string-matching stderr.
    pub async fn load(
        &self,
        uuid: &str,
        opts: &LoadOptions,
    ) -> Result<serde_json::Value, VmadmError> {
        let output = run(&self.vmadm_bin, &["get", uuid]).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr_matches_no_such_zone(&stderr) {
                return Err(VmadmError::NotFound {
                    uuid: uuid.to_string(),
                });
            }
            return Err(VmadmError::NonZeroExit {
                status: output.status,
                stderr: stderr.into_owned(),
            });
        }

        let mut vm: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|source| VmadmError::Parse { source })?;

        // do_not_inventory handling: unless callers explicitly asked for
        // DNI VMs, we pretend these don't exist.
        let dni = vm
            .get("do_not_inventory")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if dni && !opts.include_dni {
            return Err(VmadmError::NotFound {
                uuid: uuid.to_string(),
            });
        }

        if let Some(fields) = &opts.fields {
            retain_fields(&mut vm, fields);
        }

        Ok(vm)
    }

    /// `vmadm info <uuid> [types]` → raw stdout (already JSON text).
    ///
    /// The legacy task returned `stdout` verbatim, not the parsed object —
    /// some callers rely on that. We parse it to validate JSON-ness while
    /// returning the decoded value.
    pub async fn info(
        &self,
        uuid: &str,
        opts: &InfoOptions,
    ) -> Result<serde_json::Value, VmadmError> {
        // Node-vmadm's `info` calls `ifExists` first so we give callers a
        // `NotFound` error rather than the raw vmadm stderr.
        self.assert_exists(uuid, opts.include_dni).await?;

        let mut args: Vec<String> = vec!["info".to_string(), uuid.to_string()];
        if !opts.types.is_empty() {
            args.push(opts.types.join(","));
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let output = run(&self.vmadm_bin, &arg_refs).await?;
        if !output.status.success() {
            return Err(VmadmError::NonZeroExit {
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        serde_json::from_slice(&output.stdout).map_err(|source| VmadmError::Parse { source })
    }

    /// `vmadm lookup -j [-o fields] key=val ...`.
    ///
    /// Filters out `do_not_inventory=true` entries unless `include_dni` is
    /// set. Matches how node-vmadm ensures DNI-filtered VMs never leak into
    /// callers that didn't opt in.
    pub async fn lookup(
        &self,
        search: &BTreeMap<String, String>,
        opts: &LookupOptions,
    ) -> Result<Vec<serde_json::Value>, VmadmError> {
        // Always request do_not_inventory so we can filter locally without
        // a second vmadm call, matching AGENT-953 behavior.
        let mut fields = opts.fields.clone();
        if let Some(ref mut f) = fields
            && !f.iter().any(|x| x == "do_not_inventory")
        {
            f.push("do_not_inventory".to_string());
        }

        let mut args: Vec<String> = vec!["lookup".to_string(), "-j".to_string()];
        if let Some(ref f) = fields {
            args.push("-o".to_string());
            args.push(f.join(","));
        }
        for (k, v) in search {
            args.push(format!("{k}={v}"));
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let output = run(&self.vmadm_bin, &arg_refs).await?;
        if !output.status.success() {
            return Err(VmadmError::NonZeroExit {
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let all: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)
            .map_err(|source| VmadmError::Parse { source })?;

        let filtered = if opts.include_dni {
            all
        } else {
            all.into_iter()
                .filter(|vm| {
                    !vm.get("do_not_inventory")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .collect()
        };

        Ok(filtered)
    }

    /// `vmadm start <uuid> [cdrom=... disk=... ...]`.
    ///
    /// `ifExists` checks the VM first, so a missing or DNI-filtered zone
    /// returns `NotFound`. If the VM is already running, returns
    /// [`VmadmError::AlreadyInState`] so callers passing `idempotent=true`
    /// can treat it as success.
    pub async fn start(&self, uuid: &str, opts: &StartOptions) -> Result<(), VmadmError> {
        self.assert_exists(uuid, opts.include_dni).await?;

        let mut args: Vec<String> = vec!["start".to_string(), uuid.to_string()];
        for v in &opts.cdrom {
            args.push(format!("cdrom={v}"));
        }
        for v in &opts.disk {
            args.push(format!("disk={v}"));
        }
        for v in &opts.order {
            args.push(format!("order={v}"));
        }
        for v in &opts.once {
            args.push(format!("once={v}"));
        }

        self.run_mutation(
            &args,
            uuid,
            "running",
            &["already running", "already booted"],
        )
        .await
    }

    /// `vmadm stop <uuid> [-F] [-t N]`.
    pub async fn stop(&self, uuid: &str, opts: &StopOptions) -> Result<(), VmadmError> {
        self.assert_exists(uuid, opts.include_dni).await?;

        let mut args: Vec<String> = vec!["stop".to_string(), uuid.to_string()];
        if opts.force {
            args.push("-F".to_string());
        }
        if let Some(t) = opts.timeout {
            args.push("-t".to_string());
            args.push(t.to_string());
        }

        self.run_mutation(&args, uuid, "stopped", &["not running", "no such process"])
            .await
    }

    /// `vmadm reboot <uuid> [-F]`.
    pub async fn reboot(&self, uuid: &str, opts: &RebootOptions) -> Result<(), VmadmError> {
        self.assert_exists(uuid, opts.include_dni).await?;

        let mut args: Vec<String> = vec!["reboot".to_string(), uuid.to_string()];
        if opts.force {
            args.push("-F".to_string());
        }

        self.run_mutation(&args, uuid, "running", &["not running"])
            .await
    }

    /// `vmadm kill [-s signal] <uuid>`.
    ///
    /// Maps `AlreadyInState` for already-stopped VMs so idempotent callers
    /// can ignore the failure.
    pub async fn kill(&self, uuid: &str, opts: &KillOptions) -> Result<(), VmadmError> {
        self.assert_exists(uuid, opts.include_dni).await?;

        let mut args: Vec<String> = vec!["kill".to_string()];
        if let Some(signal) = &opts.signal {
            args.push("-s".to_string());
            args.push(signal.clone());
        }
        args.push(uuid.to_string());

        self.run_mutation(&args, uuid, "stopped", &["not running", "no such process"])
            .await
    }

    /// Run a vmadm mutation that produces no stdout, translating the
    /// "already in target state" stderr patterns into
    /// [`VmadmError::AlreadyInState`].
    async fn run_mutation(
        &self,
        args: &[String],
        uuid: &str,
        target_state: &'static str,
        idempotent_patterns: &[&str],
    ) -> Result<(), VmadmError> {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = run(&self.vmadm_bin, &arg_refs).await?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_lc = stderr.to_lowercase();
        if idempotent_patterns
            .iter()
            .any(|p| stderr_lc.contains(&p.to_lowercase()))
        {
            return Err(VmadmError::AlreadyInState {
                uuid: uuid.to_string(),
                state: target_state,
            });
        }
        Err(VmadmError::NonZeroExit {
            status: output.status,
            stderr: stderr.into_owned(),
        })
    }

    /// Internal helper: calls `vmadm get` just to confirm the VM exists.
    ///
    /// Mirrors `ifExists` in node-vmadm. Returns `Err(NotFound)` if the VM
    /// is missing or DNI-filtered.
    async fn assert_exists(&self, uuid: &str, include_dni: bool) -> Result<(), VmadmError> {
        let opts = LoadOptions {
            include_dni,
            fields: None,
        };
        self.load(uuid, &opts).await.map(|_| ())
    }
}

/// Spawn `bin` with `args` and collect its exit status / stdout / stderr.
async fn run(bin: &Path, args: &[&str]) -> Result<std::process::Output, VmadmError> {
    tokio::process::Command::new(bin)
        .args(args)
        .output()
        .await
        .map_err(|source| VmadmError::Spawn {
            path: bin.to_path_buf(),
            source,
        })
}

/// Matches node-vmadm's heuristic for the "VM does not exist" stderr line.
fn stderr_matches_no_such_zone(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|line| line.contains("No such zone configured"))
}

/// Drop fields from `vm` that aren't in `keep`. Matches node-vmadm's
/// `opts.fields` filter in `vmLoad`.
fn retain_fields(vm: &mut serde_json::Value, keep: &[String]) {
    if let serde_json::Value::Object(map) = vm {
        map.retain(|k, _| keep.iter().any(|f| f == k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_no_such_zone_detected() {
        let stderr = "some noise\nzone 'abc' failed: No such zone configured\n";
        assert!(stderr_matches_no_such_zone(stderr));
    }

    #[test]
    fn stderr_other_errors_ignored() {
        assert!(!stderr_matches_no_such_zone("Cannot obtain lock\n"));
    }

    #[test]
    fn retain_fields_drops_unlisted() {
        let mut vm = serde_json::json!({
            "uuid": "abc",
            "state": "running",
            "ram": 2048,
            "internal": "x"
        });
        retain_fields(&mut vm, &["uuid".to_string(), "state".to_string()]);
        let obj = vm.as_object().expect("object");
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("uuid"));
        assert!(obj.contains_key("state"));
        assert!(!obj.contains_key("internal"));
    }
}
