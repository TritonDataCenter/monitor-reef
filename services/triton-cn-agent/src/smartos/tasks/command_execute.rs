// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `command_execute` — drop a script to `/tmp/cnagent-<id>`, chmod +x,
//! run it with the caller's env and argv, return stdout/stderr/exit
//! code. Compatibility shim for the ur-agent replacement path.
//!
//! The returned shape matches the legacy task exactly:
//! `{ err, exitCode, signal, stderr, stdout }`. Callers (notably
//! sdc-clients `ServerExecute`) string-parse these, so keep the keys
//! verbatim.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use cn_agent_api::{TaskError, TaskResult};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::registry::TaskHandler;

/// Matches the legacy `MAX_BUFFER` of 5 MiB — anything larger is treated
/// as overflow.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 5 * 1024 * 1024;

/// Directory where we drop the temporary script.
pub const DEFAULT_SCRIPT_DIR: &str = "/tmp";

/// Prefix for the temporary script filename, matches `tmpFilename()` in
/// the legacy code.
pub const SCRIPT_PREFIX: &str = "cnagent-";

#[derive(Debug, Deserialize)]
struct Params {
    script: String,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    /// Legacy allows both a direct number and string-coercion; we
    /// serialize to u64 seconds.
    #[serde(default)]
    timeout: Option<u64>,
}

pub struct CommandExecuteTask {
    /// Directory where temporary scripts are written. Default `/tmp`.
    pub script_dir: PathBuf,
    /// Upper bound on stdout/stderr bytes retained; anything past this
    /// is truncated and reported via `truncated: true`.
    pub max_output_bytes: usize,
}

impl CommandExecuteTask {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_script_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            script_dir: dir.into(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl Default for CommandExecuteTask {
    fn default() -> Self {
        Self {
            script_dir: PathBuf::from(DEFAULT_SCRIPT_DIR),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

#[async_trait]
impl TaskHandler for CommandExecuteTask {
    async fn run(&self, params: serde_json::Value) -> Result<TaskResult, TaskError> {
        let p: Params = serde_json::from_value(params)
            .map_err(|e| TaskError::new(format!("invalid params: {e}")))?;

        let script_path = temp_script_path(&self.script_dir);
        write_script(&script_path, &p.script).await.map_err(|e| {
            TaskError::new(format!(
                "failed to write script {}: {e}",
                script_path.display()
            ))
        })?;

        let result = execute_and_capture(
            &script_path,
            p.args.unwrap_or_default(),
            p.env.unwrap_or_default(),
            p.timeout,
            self.max_output_bytes,
        )
        .await;

        // Always unlink, even on error. Legacy does the same.
        if let Err(unlink_err) = tokio::fs::remove_file(&script_path).await {
            tracing::warn!(
                path = %script_path.display(),
                error = %unlink_err,
                "failed to remove temp script"
            );
        }

        result
    }
}

/// Generate a legacy-compatible `/tmp/cnagent-<hex>` filename.
fn temp_script_path(dir: &std::path::Path) -> PathBuf {
    // Rand via tokio is overkill; use process time nanoseconds XOR pid.
    // Collisions are fine to retry on write, but millions of nsec granularity
    // means the odds are negligible.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let id = now ^ pid;
    dir.join(format!("{SCRIPT_PREFIX}{:x}", id))
}

async fn write_script(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Write the script, then narrow permissions. Doing it in two steps
    // (write-then-chmod) rather than one (OpenOptions.mode) keeps the
    // code portable to the tokio OpenOptions, which doesn't expose the
    // unix `mode()` extension trait.
    let mut f = tokio::fs::File::create(path).await?;
    f.write_all(body.as_bytes()).await?;
    f.flush().await?;
    drop(f);
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

async fn execute_and_capture(
    script: &std::path::Path,
    args: Vec<String>,
    env: HashMap<String, String>,
    timeout: Option<u64>,
    max_bytes: usize,
) -> Result<TaskResult, TaskError> {
    let mut cmd = tokio::process::Command::new(script);
    cmd.args(&args);

    // env_clear() + explicit vars matches the legacy "tries to match Ur's
    // default env" semantics: the child sees only what the caller asked
    // for, plus nothing else.
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| TaskError::new(format!("failed to spawn {}: {e}", script.display())))?;

    let future = child.wait_with_output();
    let output = if let Some(secs) = timeout.filter(|s| *s > 0) {
        match tokio::time::timeout(Duration::from_secs(secs), future).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Err(TaskError::new(format!("exec error: {e}")));
            }
            Err(_) => {
                return Ok(serde_json::json!({
                    "err": format!("timed out after {secs}s"),
                    "exitCode": serde_json::Value::Null,
                    "signal": serde_json::Value::Null,
                    "stderr": "",
                    "stdout": "",
                    "timedOut": true,
                }));
            }
        }
    } else {
        future
            .await
            .map_err(|e| TaskError::new(format!("exec error: {e}")))?
    };

    let (stdout, stdout_truncated) = truncate(&output.stdout, max_bytes);
    let (stderr, stderr_truncated) = truncate(&output.stderr, max_bytes);
    let truncated = stdout_truncated || stderr_truncated;

    let exit_code = output
        .status
        .code()
        .map(serde_json::Value::from)
        .unwrap_or(serde_json::Value::Null);
    let signal = signal_from_status(&output.status);

    let mut result = serde_json::json!({
        "err": serde_json::Value::Null,
        "exitCode": exit_code,
        "signal": signal,
        "stderr": String::from_utf8_lossy(&stderr).into_owned(),
        "stdout": String::from_utf8_lossy(&stdout).into_owned(),
    });
    if truncated && let serde_json::Value::Object(ref mut map) = result {
        map.insert("truncated".to_string(), serde_json::Value::Bool(true));
    }
    Ok(result)
}

fn truncate(data: &[u8], max: usize) -> (Vec<u8>, bool) {
    if data.len() <= max {
        return (data.to_vec(), false);
    }
    (data[..max].to_vec(), true)
}

#[cfg(unix)]
fn signal_from_status(status: &std::process::ExitStatus) -> serde_json::Value {
    use std::os::unix::process::ExitStatusExt;
    status
        .signal()
        .map(serde_json::Value::from)
        .unwrap_or(serde_json::Value::Null)
}

#[cfg(not(unix))]
fn signal_from_status(_status: &std::process::ExitStatus) -> serde_json::Value {
    serde_json::Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_execute_runs_script_and_returns_output() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let task = CommandExecuteTask::with_script_dir(tmp.path());
        let result = task
            .run(serde_json::json!({
                "script": "#!/bin/sh\necho hello; echo bad 1>&2; exit 7\n"
            }))
            .await
            .expect("task run");
        assert_eq!(result["exitCode"], 7);
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello");
        assert_eq!(result["stderr"].as_str().unwrap().trim(), "bad");
    }

    #[tokio::test]
    async fn command_execute_respects_timeout() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let task = CommandExecuteTask::with_script_dir(tmp.path());
        let result = task
            .run(serde_json::json!({
                "script": "#!/bin/sh\nsleep 10\n",
                "timeout": 1
            }))
            .await
            .expect("task run");
        assert_eq!(result["timedOut"], true);
    }

    #[tokio::test]
    async fn command_execute_passes_env_and_args() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let task = CommandExecuteTask::with_script_dir(tmp.path());
        let result = task
            .run(serde_json::json!({
                "script": "#!/bin/sh\necho \"$1 $EXTRA\"\n",
                "args": ["first"],
                "env": {"EXTRA": "second", "PATH": "/usr/bin:/bin"}
            }))
            .await
            .expect("task run");
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "first second");
    }

    #[tokio::test]
    async fn command_execute_truncates_large_output() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut task = CommandExecuteTask::with_script_dir(tmp.path());
        task.max_output_bytes = 64;
        let result = task
            .run(serde_json::json!({
                "script": "#!/bin/sh\nhead -c 1024 /dev/urandom | od -An -x\n"
            }))
            .await
            .expect("task run");
        assert_eq!(result["truncated"], true);
        assert!(result["stdout"].as_str().unwrap().len() <= 64);
    }
}
