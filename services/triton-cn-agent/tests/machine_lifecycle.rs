// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests for the four vmadm lifecycle tasks.
//!
//! A single shell script stands in for `/usr/sbin/vmadm`: it dispatches on
//! the subcommand (`get`/`start`/`stop`/`reboot`/`kill`) and returns
//! scripted output. Tests verify both the happy path (mutation succeeds +
//! VM reload returns the expected shape) and idempotent behavior (already-
//! running / not-running stderr ↔ `idempotent=true`).

#![allow(clippy::expect_used)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cn_agent_api::{TaskName, TaskRequest, cn_agent_api_mod, types::Uuid};
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServer, HttpServerStarter};
use triton_cn_agent::{
    AgentContext, AgentMetadata,
    api_impl::CnAgentApiImpl,
    registry::TaskRegistry,
    smartos::VmadmTool,
    smartos::tasks::machine_lifecycle::{
        MachineBootTask, MachineKillTask, MachineRebootTask, MachineShutdownTask,
    },
};

const RUNNING_VM: &str = r#"{
    "uuid": "abc00000-1111-2222-3333-444444444444",
    "state": "running",
    "brand": "joyent"
}"#;

const STOPPED_VM: &str = r#"{
    "uuid": "abc00000-1111-2222-3333-444444444444",
    "state": "stopped",
    "brand": "joyent"
}"#;

fn write_script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("vmadm");
    let mut f = std::fs::File::create(&path).expect("create script");
    f.write_all(body.as_bytes()).expect("write script");
    let mut perms = f.metadata().expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// Build a mock vmadm that:
///   * returns `get_vm_json` on `vmadm get`
///   * exits 0 for `start/stop/reboot/kill` (no stderr)
///   * exits 1 with `idempotent_stderr` if `fail_mutation` is true and the
///     invocation is one of start/stop/reboot/kill.
fn build_mock(get_vm_json: &str, fail_mutation: bool, idempotent_stderr: &str) -> String {
    let mutation_arm = if fail_mutation {
        format!("start|stop|reboot|kill) echo '{idempotent_stderr}' 1>&2; exit 1 ;;")
    } else {
        "start|stop|reboot|kill) exit 0 ;;".to_string()
    };
    format!(
        "#!/bin/sh\ncase \"$1\" in\n  get) cat <<'JSON'\n{get_vm_json}\nJSON\n    ;;\n  {mutation_arm}\n  *) exit 1 ;;\nesac\n"
    )
}

async fn start_agent(registry: TaskRegistry) -> (HttpServer<Arc<AgentContext>>, String) {
    let metadata = AgentMetadata {
        name: "cn-agent".to_string(),
        version: "test".to_string(),
        server_uuid: Uuid::nil(),
        backend: "smartos".to_string(),
    };
    let context = Arc::new(AgentContext::new(metadata, registry));
    let api = cn_agent_api_mod::api_description::<CnAgentApiImpl>().expect("api");
    let config = ConfigDropshot {
        bind_address: "127.0.0.1:0".parse().expect("bind"),
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };
    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Warn,
    }
    .to_logger("cn-agent-lifecycle-test")
    .expect("logger");

    let server = HttpServerStarter::new(&config, api, context, &log)
        .expect("server starter")
        .start();
    let url = format!("http://{}", server.local_addr());
    (server, url)
}

fn register_lifecycle(tool: Arc<VmadmTool>) -> TaskRegistry {
    TaskRegistry::builder()
        .register(TaskName::MachineBoot, MachineBootTask::new(tool.clone()))
        .register(
            TaskName::MachineShutdown,
            MachineShutdownTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineReboot,
            MachineRebootTask::new(tool.clone()),
        )
        .register(TaskName::MachineKill, MachineKillTask::new(tool))
        .build()
}

const UUID: &str = "abc00000-1111-2222-3333-444444444444";

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn machine_boot_returns_vm_after_start() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = write_script(tmp.path(), &build_mock(RUNNING_VM, false, ""));
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(register_lifecycle(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineBoot,
            params: serde_json::json!({"uuid": UUID}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["vm"]["uuid"], UUID);
    assert_eq!(body["vm"]["state"], "running");

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_shutdown_returns_vm_after_stop() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = write_script(tmp.path(), &build_mock(STOPPED_VM, false, ""));
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(register_lifecycle(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineShutdown,
            params: serde_json::json!({"uuid": UUID, "force": true, "timeout": 30}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["vm"]["state"], "stopped");

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_reboot_returns_vm_after_reboot() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = write_script(tmp.path(), &build_mock(RUNNING_VM, false, ""));
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(register_lifecycle(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineReboot,
            params: serde_json::json!({"uuid": UUID, "force": true}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["vm"]["state"], "running");

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_kill_returns_vm_after_kill() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = write_script(tmp.path(), &build_mock(STOPPED_VM, false, ""));
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(register_lifecycle(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineKill,
            params: serde_json::json!({"uuid": UUID, "signal": "TERM"}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["vm"]["state"], "stopped");

    server.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Idempotent paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn machine_boot_is_idempotent_when_flag_set() {
    // vmadm start fails with "already running"; with idempotent=true we
    // should succeed and reload the already-running VM.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = write_script(
        tmp.path(),
        &build_mock(RUNNING_VM, true, "VM is already running"),
    );
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(register_lifecycle(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineBoot,
            params: serde_json::json!({"uuid": UUID, "idempotent": true}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["vm"]["state"], "running");

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_boot_without_idempotent_propagates_error() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = write_script(
        tmp.path(),
        &build_mock(RUNNING_VM, true, "VM is already running"),
    );
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(register_lifecycle(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineBoot,
            params: serde_json::json!({"uuid": UUID}),
        })
        .send()
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or("")
            .contains("already running"),
        "unexpected message: {}",
        body["message"]
    );

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_shutdown_is_idempotent_when_not_running() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = write_script(
        tmp.path(),
        &build_mock(STOPPED_VM, true, "VM is not running"),
    );
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(register_lifecycle(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineShutdown,
            params: serde_json::json!({"uuid": UUID, "idempotent": true}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["vm"]["state"], "stopped");

    server.close().await.expect("close");
}
