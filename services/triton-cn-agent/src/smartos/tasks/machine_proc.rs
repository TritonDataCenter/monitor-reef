// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `machine_proc` — dump the per-process table for a running zone.
//!
//! The legacy task uses the `procread` native addon to walk
//! `/proc/<pid>/psinfo` inside the zone and pull every field. We avoid
//! binary parsing of `psinfo_t` by delegating to illumos `ps`, which
//! does the kernel-side filtering for us via `-z <zone>` and emits a
//! stable set of column values. The resulting shape matches what the
//! legacy code returned (map keyed by pid, with a nested `psinfo`
//! object).
//!
//! **LX-brand remapping**: the legacy task rewrites init's pid to 1 and
//! zsched's pid to 0 so CNAPI sees the "inside the zone" numbering
//! instead of the global-zone numbering. We reproduce that here.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;

use crate::registry::TaskHandler;
use crate::smartos::tasks::machine_load::vmadm_error_to_task;
use crate::smartos::vmadm::{LoadOptions, VmadmTool};

pub const DEFAULT_PS_BIN: &str = "/usr/bin/ps";

/// Columns we ask illumos ps to emit. Ordering here determines how we
/// parse the output; the field list deliberately matches what the
/// legacy procread addon surfaced for each psinfo entry we care about.
const PS_FIELDS: &[&str] = &[
    "pid", "ppid", "pcpu", "pmem", "vsz", "rss", "s", "time", "etime", "uid", "ruid", "fname",
];

#[derive(Debug, Deserialize)]
struct Params {
    uuid: String,
    #[serde(default)]
    include_dni: Option<bool>,
}

pub struct MachineProcTask {
    vmadm: Arc<VmadmTool>,
    ps_bin: PathBuf,
}

impl MachineProcTask {
    pub fn new(vmadm: Arc<VmadmTool>) -> Self {
        Self {
            vmadm,
            ps_bin: PathBuf::from(DEFAULT_PS_BIN),
        }
    }

    pub fn with_ps_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.ps_bin = bin.into();
        self
    }
}

#[async_trait]
impl TaskHandler for MachineProcTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;
        let include_dni = p.include_dni.unwrap_or(false);

        // Look up the VM to validate existence + collect brand/init pid
        // for the LX remapping.
        let load_opts = LoadOptions {
            include_dni,
            fields: Some(
                ["brand", "pid", "zone_state"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        };
        let vm = self
            .vmadm
            .load(&p.uuid, &load_opts)
            .await
            .map_err(|e| vmadm_error_to_task(e, "vmadm.load error"))?;

        let zone_state = vm.get("zone_state").and_then(|v| v.as_str()).unwrap_or("");
        if zone_state != "running" {
            let mut err = TaskError::new("VM is not running".to_string());
            err.rest_code = Some("VmNotRunning".to_string());
            return Err(err);
        }

        let brand = vm
            .get("brand")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let init_pid = vm.get("pid").and_then(|v| v.as_u64());

        let rows = self.run_ps(&p.uuid).await?;

        // Find zsched pid (init's parent) if we have init's pid, so we
        // can perform the LX-brand pid remapping.
        let zsched_pid =
            init_pid.and_then(|init| rows.iter().find(|r| r.pid == init).map(|r| r.ppid));

        let mut procs = serde_json::Map::new();
        for row in rows {
            let (pr_pid, pr_ppid) = if brand == "lx" {
                remap_lx_pid(row.pid, row.ppid, init_pid, zsched_pid)
            } else {
                (row.pid, row.ppid)
            };

            let psinfo = serde_json::json!({
                "pr_pid": pr_pid,
                "pr_ppid": pr_ppid,
                "pr_fname": row.fname,
                "pr_pctcpu": row.pcpu,
                "pr_pctmem": row.pmem,
                "pr_size": row.vsz,
                "pr_rssize": row.rss,
                "pr_state": row.state,
                "pr_time": row.time,
                "pr_etime": row.etime,
                "pr_uid": row.uid,
                "pr_ruid": row.ruid,
            });
            procs.insert(pr_pid.to_string(), serde_json::json!({ "psinfo": psinfo }));
        }

        Ok(serde_json::Value::Object(procs))
    }
}

impl MachineProcTask {
    async fn run_ps(&self, zone: &str) -> Result<Vec<PsRow>, TaskError> {
        let format = PS_FIELDS.join(",");
        let output = tokio::process::Command::new(&self.ps_bin)
            .args(["-z", zone, "-o", &format])
            .output()
            .await
            .map_err(|e| TaskError::new(format!("failed to spawn ps: {e}")))?;
        if !output.status.success() {
            return Err(TaskError::new(format!(
                "ps exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(parse_ps_output(&text))
    }
}

/// One row of `ps` output. All numeric fields are stored as raw strings
/// when they can be fractional (pcpu, pmem) or for times — mirroring
/// what CNAPI expects to see on the wire.
#[derive(Debug, Clone, PartialEq)]
struct PsRow {
    pid: u64,
    ppid: u64,
    pcpu: String,
    pmem: String,
    vsz: u64,
    rss: u64,
    state: String,
    time: String,
    etime: String,
    uid: u64,
    ruid: u64,
    fname: String,
}

/// Parse illumos `ps -o pid,ppid,pcpu,pmem,vsz,rss,s,time,etime,uid,ruid,fname`
/// output. First line is the header, which we discard.
fn parse_ps_output(text: &str) -> Vec<PsRow> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if idx == 0 {
            // header row
            continue;
        }
        // illumos ps right-justifies fixed-width columns. split_whitespace
        // collapses runs of spaces, which is what we want.
        let cols: Vec<&str> = line.split_whitespace().collect();
        // 12 columns expected; fname may itself have whitespace if the
        // binary is weird but illumos ps uses the shortened file name.
        if cols.len() < PS_FIELDS.len() {
            tracing::warn!(
                "ps row has {} cols, expected {}: {line:?}",
                cols.len(),
                PS_FIELDS.len()
            );
            continue;
        }
        let Ok(pid) = cols[0].parse::<u64>() else {
            continue;
        };
        let Ok(ppid) = cols[1].parse::<u64>() else {
            continue;
        };
        let vsz = cols[4].parse::<u64>().unwrap_or(0);
        let rss = cols[5].parse::<u64>().unwrap_or(0);
        let uid = cols[9].parse::<u64>().unwrap_or(0);
        let ruid = cols[10].parse::<u64>().unwrap_or(0);
        // Fname is the last column; if the name had internal whitespace
        // (unusual), rejoin the tail to be safe.
        let fname = cols[11..].join(" ");
        out.push(PsRow {
            pid,
            ppid,
            pcpu: cols[2].to_string(),
            pmem: cols[3].to_string(),
            vsz,
            rss,
            state: cols[6].to_string(),
            time: cols[7].to_string(),
            etime: cols[8].to_string(),
            uid,
            ruid,
            fname,
        });
    }
    out
}

/// For LX-branded zones, rewrite init's pid to 1 and zsched's to 0.
///
/// The legacy task also sets pr_ppid to 0 when pr_pid is zsched's, so
/// zsched shows up as its own "parent"-less entry inside the zone. We
/// replicate that exactly — see machine_proc.js lines 82-98.
fn remap_lx_pid(pid: u64, ppid: u64, init_pid: Option<u64>, zsched_pid: Option<u64>) -> (u64, u64) {
    let Some(init) = init_pid else {
        return (pid, ppid);
    };
    let new_pid = if Some(pid) == zsched_pid {
        0
    } else if pid == init {
        1
    } else {
        pid
    };
    let new_ppid = if Some(ppid) == zsched_pid {
        0
    } else if ppid == init {
        1
    } else if Some(pid) == zsched_pid {
        // zsched's parent is always 0 in-zone view.
        0
    } else {
        ppid
    };
    (new_pid, new_ppid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ps_output_skips_header_and_parses_rows() {
        let text = "  PID  PPID %CPU %MEM  VSZ  RSS S     TIME     ELAPSED   UID  RUID COMMAND
 6611  5474  0.1  0.2 4504 2096 S    00:00 10-09:49:26     0     0 inetd
 5729  5474  0.0  0.0 2648 1660 S    00:00 10-09:49:33     0     0 pfexecd";
        let rows = parse_ps_output(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].pid, 6611);
        assert_eq!(rows[0].ppid, 5474);
        assert_eq!(rows[0].pcpu, "0.1");
        assert_eq!(rows[0].vsz, 4504);
        assert_eq!(rows[0].fname, "inetd");
    }

    #[test]
    fn remap_lx_pid_rewrites_init_and_zsched() {
        // For an LX zone with init_pid=5474, zsched_pid=4213:
        let init = Some(5474);
        let zsched = Some(4213);
        assert_eq!(remap_lx_pid(5474, 4213, init, zsched), (1, 0));
        assert_eq!(remap_lx_pid(4213, 1, init, zsched), (0, 0));
        // Arbitrary child of init keeps its own pid but ppid remaps to 1.
        assert_eq!(remap_lx_pid(6611, 5474, init, zsched), (6611, 1));
    }

    #[test]
    fn remap_lx_pid_no_op_without_init() {
        // If we don't know init, just pass through.
        assert_eq!(remap_lx_pid(42, 7, None, None), (42, 7));
    }
}
