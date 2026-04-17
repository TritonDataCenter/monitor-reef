// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests for the read-only `machine_*` tasks (machine_load, machine_info).
//!
//! A shell script stands in for `/usr/sbin/vmadm`. It dispatches on the first
//! argument (`get` vs `info`) so one mock can serve both machine_load and
//! the `ifExists` precheck that machine_info performs.

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
    smartos::tasks::{machine_info::MachineInfoTask, machine_load::MachineLoadTask},
};

/// Write a shell script and chmod +x.
fn write_script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("vmadm");
    let mut f = std::fs::File::create(&path).expect("create script");
    f.write_all(body.as_bytes()).expect("write script");
    let mut perms = f.metadata().expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

async fn start_agent_with(registry: TaskRegistry) -> (HttpServer<Arc<AgentContext>>, String) {
    let metadata = AgentMetadata {
        name: "cn-agent".to_string(),
        version: "test".to_string(),
        server_uuid: Uuid::nil(),
        backend: "smartos".to_string(),
    };
    let context = Arc::new(AgentContext::new(metadata, registry));
    let api = cn_agent_api_mod::api_description::<CnAgentApiImpl>().expect("api");
    let config = ConfigDropshot {
        bind_address: "127.0.0.1:0".parse().expect("bind addr"),
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };
    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Warn,
    }
    .to_logger("cn-agent-machine-test")
    .expect("logger");

    let server = HttpServerStarter::new(&config, api, context, &log)
        .expect("server starter")
        .start();
    let url = format!("http://{}", server.local_addr());
    (server, url)
}

const VM_JSON: &str = r#"{
    "uuid": "abc00000-1111-2222-3333-444444444444",
    "state": "running",
    "brand": "joyent",
    "ram": 2048
}"#;

const DNI_VM_JSON: &str = r#"{
    "uuid": "dn100000-1111-2222-3333-444444444444",
    "state": "running",
    "brand": "joyent",
    "do_not_inventory": true
}"#;

const INFO_JSON: &str = r#"{
    "vnc": { "host": "10.0.0.1", "port": 5900 }
}"#;

#[tokio::test]
async fn machine_load_returns_vm_json() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    // The mock always echoes the VM JSON for a `get` subcommand.
    let script_body = format!(
        "#!/bin/sh\ncase \"$1\" in\n  get) cat <<'JSON'\n{VM_JSON}\nJSON\n    ;;\n  *) exit 1\n    ;;\nesac\n"
    );
    let bin = write_script(tmp.path(), &script_body);

    let tool = Arc::new(VmadmTool::with_bin(bin));
    let registry = TaskRegistry::builder()
        .register(TaskName::MachineLoad, MachineLoadTask::new(tool))
        .build();
    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineLoad,
            params: serde_json::json!({"uuid": "abc00000-1111-2222-3333-444444444444"}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["state"], "running");
    assert_eq!(body["ram"], 2048);

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_load_applies_fields_filter() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let script_body = format!("#!/bin/sh\ncat <<'JSON'\n{VM_JSON}\nJSON\n");
    let bin = write_script(tmp.path(), &script_body);

    let tool = Arc::new(VmadmTool::with_bin(bin));
    let registry = TaskRegistry::builder()
        .register(TaskName::MachineLoad, MachineLoadTask::new(tool))
        .build();
    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineLoad,
            params: serde_json::json!({
                "uuid": "abc00000-1111-2222-3333-444444444444",
                "fields": ["uuid", "state"]
            }),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    let obj = body.as_object().expect("object");
    assert_eq!(obj.len(), 2);
    assert!(obj.contains_key("uuid"));
    assert!(obj.contains_key("state"));

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_load_treats_missing_zone_as_not_found() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    // vmadm emits "No such zone configured" on stderr and exits non-zero.
    let script_body = "#!/bin/sh\necho 'zone xyz failed: No such zone configured' 1>&2\nexit 1\n";
    let bin = write_script(tmp.path(), script_body);

    let tool = Arc::new(VmadmTool::with_bin(bin));
    let registry = TaskRegistry::builder()
        .register(TaskName::MachineLoad, MachineLoadTask::new(tool))
        .build();
    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineLoad,
            params: serde_json::json!({"uuid": "abc00000-1111-2222-3333-444444444444"}),
        })
        .send()
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["error_code"], "VmNotFound");
    assert!(
        body["message"].as_str().unwrap_or("").contains("not found"),
        "unexpected message: {}",
        body["message"]
    );

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_load_hides_do_not_inventory_vms_by_default() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let script_body = format!("#!/bin/sh\ncat <<'JSON'\n{DNI_VM_JSON}\nJSON\n");
    let bin = write_script(tmp.path(), &script_body);

    let tool = Arc::new(VmadmTool::with_bin(bin));
    let registry = TaskRegistry::builder()
        .register(TaskName::MachineLoad, MachineLoadTask::new(tool))
        .build();
    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    // Without include_dni, the VM should appear as not-found.
    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineLoad,
            params: serde_json::json!({"uuid": "dn100000-1111-2222-3333-444444444444"}),
        })
        .send()
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["error_code"], "VmNotFound");

    // With include_dni, it should load.
    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineLoad,
            params: serde_json::json!({
                "uuid": "dn100000-1111-2222-3333-444444444444",
                "include_dni": true
            }),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["do_not_inventory"], true);

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_info_calls_ifexists_then_info() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    // Dispatch: `vmadm get` returns the VM JSON (for ifExists) and
    // `vmadm info` returns info JSON.
    let script_body = format!(
        "#!/bin/sh\ncase \"$1\" in\n  \
        get) cat <<'JSON'\n{VM_JSON}\nJSON\n    ;;\n  \
        info) cat <<'JSON'\n{INFO_JSON}\nJSON\n    ;;\n  \
        *) exit 1\n    ;;\nesac\n"
    );
    let bin = write_script(tmp.path(), &script_body);

    let tool = Arc::new(VmadmTool::with_bin(bin));
    let registry = TaskRegistry::builder()
        .register(TaskName::MachineInfo, MachineInfoTask::new(tool))
        .build();
    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineInfo,
            params: serde_json::json!({"uuid": "abc00000-1111-2222-3333-444444444444"}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["vnc"]["port"], 5900);

    server.close().await.expect("close");
}

#[tokio::test]
async fn machine_info_404s_when_vm_missing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    // ifExists (vmadm get) fails → machine_info should surface VmNotFound
    // without calling `vmadm info`.
    let script_body = "#!/bin/sh\necho 'No such zone configured' 1>&2\nexit 1\n";
    let bin = write_script(tmp.path(), script_body);

    let tool = Arc::new(VmadmTool::with_bin(bin));
    let registry = TaskRegistry::builder()
        .register(TaskName::MachineInfo, MachineInfoTask::new(tool))
        .build();
    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::MachineInfo,
            params: serde_json::json!({"uuid": "ff000000-1111-2222-3333-444444444444"}),
        })
        .send()
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["error_code"], "VmNotFound");

    server.close().await.expect("close");
}
