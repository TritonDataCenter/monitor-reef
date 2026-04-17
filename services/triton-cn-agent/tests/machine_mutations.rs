// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Integration tests for `machine_destroy`, `machine_update`, and the
//! three `machine_*_snapshot` tasks.
//!
//! The mock vmadm dispatches on $1 (`get`, `delete`, `update`,
//! `create-snapshot`, `delete-snapshot`, `rollback-snapshot`) and records
//! its argv + stdin for assertions.

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
    smartos::tasks::{
        machine_destroy::MachineDestroyTask,
        machine_snapshots::{
            MachineCreateSnapshotTask, MachineDeleteSnapshotTask, MachineRollbackSnapshotTask,
        },
        machine_update::MachineUpdateTask,
    },
};

const RUNNING_VM: &str = r#"{
    "uuid": "abc00000-1111-2222-3333-444444444444",
    "state": "running",
    "brand": "joyent"
}"#;

fn write_script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("vmadm");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    let mut perms = f.metadata().expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
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
    .to_logger("cn-agent-mutation-test")
    .expect("logger");

    let server = HttpServerStarter::new(&config, api, context, &log)
        .expect("server")
        .start();
    let url = format!("http://{}", server.local_addr());
    (server, url)
}

fn all_registry(tool: Arc<VmadmTool>) -> TaskRegistry {
    TaskRegistry::builder()
        .register(
            TaskName::MachineDestroy,
            MachineDestroyTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineUpdate,
            MachineUpdateTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineCreateSnapshot,
            MachineCreateSnapshotTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineDeleteSnapshot,
            MachineDeleteSnapshotTask::new(tool.clone()),
        )
        .register(
            TaskName::MachineRollbackSnapshot,
            MachineRollbackSnapshotTask::new(tool),
        )
        .build()
}

const UUID: &str = "abc00000-1111-2222-3333-444444444444";

// ---------------------------------------------------------------------------
// Destroy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn machine_destroy_happy_path() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    // `vmadm get` returns the VM (for ifExists); `vmadm delete` succeeds.
    let body = format!(
        "#!/bin/sh\ncase \"$1\" in\n  get) cat <<'JSON'\n{RUNNING_VM}\nJSON\n    ;;\n  delete) exit 0 ;;\n  *) exit 1 ;;\nesac\n"
    );
    let bin = write_script(tmp.path(), &body);
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(all_registry(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineDestroy,
            params: serde_json::json!({"uuid": UUID}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body, serde_json::json!({}));

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_destroy_is_idempotent_when_vm_missing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    // `vmadm get` exits non-zero with "No such zone configured" on stderr,
    // mirroring what the real vmadm emits when the VM is already gone.
    let body = "#!/bin/sh\ncase \"$1\" in\n  get) echo 'No such zone configured' 1>&2; exit 1 ;;\n  *) exit 0 ;;\nesac\n";
    let bin = write_script(tmp.path(), body);
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(all_registry(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineDestroy,
            params: serde_json::json!({"uuid": UUID}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(
        resp.status().is_success(),
        "destroying a missing VM should succeed idempotently; got status {}",
        resp.status()
    );

    server.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn machine_update_returns_reloaded_vm() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let stdin_log = tmp.path().join("stdin.log");
    // `vmadm get` returns RUNNING_VM, `vmadm update` reads stdin and
    // writes it to stdin.log, `vmadm reboot` is a no-op. (No NIC changes
    // in the payload, so reboot shouldn't actually be invoked — we keep
    // the arm here just to avoid exit 1 if the test accidentally calls it.)
    let body = format!(
        "#!/bin/sh\ncase \"$1\" in\n  get) cat <<'JSON'\n{RUNNING_VM}\nJSON\n    ;;\n  update) cat > {stdin} ;;\n  reboot) exit 0 ;;\n  *) exit 1 ;;\nesac\n",
        stdin = stdin_log.display()
    );
    let bin = write_script(tmp.path(), &body);
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(all_registry(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineUpdate,
            params: serde_json::json!({
                "uuid": UUID,
                "ram": 4096,
                "include_dni": false
            }),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["vm"]["uuid"], UUID);

    // Verify vmadm got the right stdin payload. Key "include_dni" should
    // be scrubbed; "ram" should come through.
    let stdin = std::fs::read_to_string(&stdin_log).expect("read stdin log");
    let parsed: serde_json::Value = serde_json::from_str(&stdin).expect("valid json");
    assert_eq!(parsed["ram"], 4096);
    assert_eq!(parsed["uuid"], UUID);
    assert!(
        parsed.get("include_dni").is_none(),
        "include_dni should be scrubbed from the vmadm payload: {parsed}"
    );

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_update_reboots_on_add_nics_when_running() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let actions = tmp.path().join("actions.log");
    let body = format!(
        "#!/bin/sh\necho \"$1\" >> {actions}\ncase \"$1\" in\n  get) cat <<'JSON'\n{RUNNING_VM}\nJSON\n    ;;\n  update) cat > /dev/null ;;\n  reboot) exit 0 ;;\n  *) exit 1 ;;\nesac\n",
        actions = actions.display()
    );
    let bin = write_script(tmp.path(), &body);
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(all_registry(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineUpdate,
            params: serde_json::json!({
                "uuid": UUID,
                "add_nics": [{"nic_tag": "admin", "ip": "dhcp"}]
            }),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());

    let log = std::fs::read_to_string(&actions).expect("actions log");
    // Sequence: get (ifExists), update, get (reload), reboot, get (reload
    // after reboot). Reboot must be present; ordering must be after update.
    let lines: Vec<&str> = log.lines().collect();
    let update_idx = lines
        .iter()
        .position(|l| *l == "update")
        .expect("update ran");
    let reboot_idx = lines
        .iter()
        .position(|l| *l == "reboot")
        .expect("reboot ran");
    assert!(
        reboot_idx > update_idx,
        "expected reboot to run after update, got: {lines:?}"
    );

    server.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Snapshot ops
// ---------------------------------------------------------------------------

#[tokio::test]
async fn snapshot_ops_invoke_expected_subcommands() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let actions = tmp.path().join("actions.log");
    let body = format!(
        "#!/bin/sh\necho \"$*\" >> {actions}\ncase \"$1\" in\n  get) cat <<'JSON'\n{RUNNING_VM}\nJSON\n    ;;\n  create-snapshot|delete-snapshot|rollback-snapshot) exit 0 ;;\n  *) exit 1 ;;\nesac\n",
        actions = actions.display()
    );
    let bin = write_script(tmp.path(), &body);
    let tool = Arc::new(VmadmTool::with_bin(bin));
    let (server, url) = start_agent(all_registry(tool)).await;
    let client = reqwest::Client::new();

    for (task, expected_cmd) in [
        (TaskName::MachineCreateSnapshot, "create-snapshot"),
        (TaskName::MachineDeleteSnapshot, "delete-snapshot"),
        (TaskName::MachineRollbackSnapshot, "rollback-snapshot"),
    ] {
        let resp = client
            .post(format!("{url}/tasks"))
            .json(&TaskRequest {
                task,
                params: serde_json::json!({"uuid": UUID, "snapshot_name": "snap1"}),
            })
            .send()
            .await
            .expect("dispatch");
        assert!(
            resp.status().is_success(),
            "{task:?} status: {}",
            resp.status()
        );
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["vm"]["uuid"], UUID);

        let log = std::fs::read_to_string(&actions).expect("actions log");
        let expected_line = format!("{expected_cmd} {UUID} snap1");
        assert!(
            log.lines().any(|l| l == expected_line),
            "expected {expected_line:?} in log, got:\n{log}"
        );
    }

    server.close().await.expect("close");
}
