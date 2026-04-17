// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests for ZFS query tasks.
//!
//! Writes short shell scripts that emit hand-rolled `zpool list`/`zfs list`
//! output, points the task handlers at those scripts, and asserts the parsed
//! JSON result.

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
    smartos::ZfsTool,
    smartos::tasks::{
        zfs_get_properties::ZfsGetPropertiesTask, zfs_list_datasets::ZfsListDatasetsTask,
        zfs_list_pools::ZfsListPoolsTask, zfs_list_snapshots::ZfsListSnapshotsTask,
    },
};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create script");
    f.write_all(body.as_bytes()).expect("write script");
    let mut perms = f.metadata().expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// Create a mock `zpool` or `zfs` that echoes a captured stdout and exits 0
/// regardless of arguments. Tests don't need to validate argv; they only
/// care that the parser handles the output shape correctly.
fn fixed_output_script(dir: &Path, name: &str, stdout: &str) -> PathBuf {
    let body = format!("#!/bin/sh\ncat <<'EOF'\n{stdout}\nEOF\n");
    write_script(dir, name, &body)
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
    .to_logger("cn-agent-zfs-test")
    .expect("logger");

    let server = HttpServerStarter::new(&config, api, context, &log)
        .expect("server starter")
        .start();
    let url = format!("http://{}", server.local_addr());
    (server, url)
}

#[tokio::test]
async fn zfs_list_pools_returns_parsed_rows() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    // Real `zpool list -Hp -o name,size,allocated,free,cap,health,altroot`:
    let zpool = fixed_output_script(
        tmp.path(),
        "zpool",
        "zones\t960197124096\t12884901888\t947312222208\t1\tONLINE\t-",
    );
    // zfs isn't called by this task, but the struct requires a path.
    let zfs = fixed_output_script(tmp.path(), "zfs", "");

    let tool = Arc::new(ZfsTool::with_bins(zfs, zpool));
    let registry = TaskRegistry::builder()
        .register(TaskName::ZfsListPools, ZfsListPoolsTask::new(tool))
        .build();

    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::ZfsListPools,
            params: serde_json::Value::Null,
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());

    let body: serde_json::Value = resp.json().await.expect("json");
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "zones");
    assert_eq!(arr[0]["health"], "ONLINE");
    assert_eq!(arr[0]["altroot"], "-");

    server.close().await.expect("close");
}

#[tokio::test]
async fn zfs_list_datasets_returns_parsed_rows() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let zpool = fixed_output_script(tmp.path(), "zpool", "");
    let zfs = fixed_output_script(
        tmp.path(),
        "zfs",
        // zfs list -Hp -o name,used,avail,refer,type,mountpoint -t all:
        "zones\t12884901888\t947312222208\t100663296\tfilesystem\t/zones\n\
         zones/images\t4194304\t947312222208\t4194304\tfilesystem\t/zones/images",
    );

    let tool = Arc::new(ZfsTool::with_bins(zfs, zpool));
    let registry = TaskRegistry::builder()
        .register(TaskName::ZfsListDatasets, ZfsListDatasetsTask::new(tool))
        .build();

    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::ZfsListDatasets,
            params: serde_json::json!({}),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "zones");
    assert_eq!(arr[0]["type"], "filesystem");
    assert_eq!(arr[1]["name"], "zones/images");

    server.close().await.expect("close");
}

#[tokio::test]
async fn zfs_list_snapshots_handles_empty_output() {
    // `zfs list -t snapshot` on a fresh box returns no rows.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let zpool = fixed_output_script(tmp.path(), "zpool", "");
    let zfs = fixed_output_script(tmp.path(), "zfs", "");

    let tool = Arc::new(ZfsTool::with_bins(zfs, zpool));
    let registry = TaskRegistry::builder()
        .register(TaskName::ZfsListSnapshots, ZfsListSnapshotsTask::new(tool))
        .build();

    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::ZfsListSnapshots,
            params: serde_json::Value::Null,
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body, serde_json::json!([]));

    server.close().await.expect("close");
}

#[tokio::test]
async fn zfs_get_properties_returns_nested_map() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let zpool = fixed_output_script(tmp.path(), "zpool", "");
    let zfs = fixed_output_script(
        tmp.path(),
        "zfs",
        // `zfs get -Hp -o name,property,value used,available zones`:
        "zones\tused\t12884901888\n\
         zones\tavailable\t947312222208",
    );

    let tool = Arc::new(ZfsTool::with_bins(zfs, zpool));
    let registry = TaskRegistry::builder()
        .register(TaskName::ZfsGetProperties, ZfsGetPropertiesTask::new(tool))
        .build();

    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::ZfsGetProperties,
            params: serde_json::json!({
                "dataset": "zones",
                "properties": ["used", "available"]
            }),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["zones"]["used"], "12884901888");
    assert_eq!(body["zones"]["available"], "947312222208");

    server.close().await.expect("close");
}

#[tokio::test]
async fn zfs_get_properties_surfaces_nonzero_exit() {
    // Script exits 1 to model `zfs get` failing on an invalid dataset.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let zpool = fixed_output_script(tmp.path(), "zpool", "");
    let zfs = write_script(
        tmp.path(),
        "zfs",
        "#!/bin/sh\necho 'cannot open does/not/exist: no such pool' 1>&2\nexit 1\n",
    );

    let tool = Arc::new(ZfsTool::with_bins(zfs, zpool));
    let registry = TaskRegistry::builder()
        .register(TaskName::ZfsGetProperties, ZfsGetPropertiesTask::new(tool))
        .build();

    let (server, url) = start_agent_with(registry).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::ZfsGetProperties,
            params: serde_json::json!({"dataset": "does/not/exist"}),
        })
        .send()
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    let msg = body["message"].as_str().expect("message");
    assert!(
        msg.contains("failed to get ZFS properties"),
        "unexpected error message: {msg}"
    );

    server.close().await.expect("close");
}
