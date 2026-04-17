// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests for the ZFS mutation tasks.
//!
//! Each test writes a tiny `zfs` stand-in that records its argv to a file
//! and either exits 0 or (for the failure test) 1 with stderr. The tasks are
//! driven through the live Dropshot server so we cover the full params→task→
//! argv→stderr chain.

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
    smartos::tasks::zfs_mutations::{
        ZfsCloneDatasetTask, ZfsCreateDatasetTask, ZfsDestroyDatasetTask, ZfsRenameDatasetTask,
        ZfsRollbackDatasetTask, ZfsSetPropertiesTask, ZfsSnapshotDatasetTask,
    },
};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    let mut perms = f.metadata().expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// Build a zfs mock that appends each invocation's argv (space-joined) to
/// `argv_log` and exits 0.
fn argv_recording_script(dir: &Path, argv_log: &Path) -> PathBuf {
    let body = format!(
        "#!/bin/sh\necho \"$*\" >> {log}\nexit 0\n",
        log = argv_log.display()
    );
    write_script(dir, "zfs", &body)
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
    .to_logger("cn-agent-zfs-mutation-test")
    .expect("logger");

    let server = HttpServerStarter::new(&config, api, context, &log)
        .expect("server")
        .start();
    let url = format!("http://{}", server.local_addr());
    (server, url)
}

fn registry_all(tool: Arc<ZfsTool>) -> TaskRegistry {
    TaskRegistry::builder()
        .register(
            TaskName::ZfsCreateDataset,
            ZfsCreateDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsDestroyDataset,
            ZfsDestroyDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsRenameDataset,
            ZfsRenameDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsSnapshotDataset,
            ZfsSnapshotDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsRollbackDataset,
            ZfsRollbackDatasetTask::new(tool.clone()),
        )
        .register(
            TaskName::ZfsCloneDataset,
            ZfsCloneDatasetTask::new(tool.clone()),
        )
        .register(TaskName::ZfsSetProperties, ZfsSetPropertiesTask::new(tool))
        .build()
}

#[tokio::test]
async fn mutations_invoke_expected_zfs_argv() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let argv_log = tmp.path().join("argv.log");
    let zfs = argv_recording_script(tmp.path(), &argv_log);
    let zpool = write_script(tmp.path(), "zpool", "#!/bin/sh\n");

    let tool = Arc::new(ZfsTool::with_bins(zfs, zpool));
    let (server, url) = start_agent(registry_all(tool)).await;
    let client = reqwest::Client::new();

    // Dispatch every mutation in one test so we verify the full argv shape.
    let cases = vec![
        (
            TaskName::ZfsCreateDataset,
            serde_json::json!({"dataset": "zones/foo"}),
            "create zones/foo",
        ),
        (
            TaskName::ZfsDestroyDataset,
            serde_json::json!({"dataset": "zones/foo"}),
            "destroy zones/foo",
        ),
        (
            TaskName::ZfsRenameDataset,
            serde_json::json!({"dataset": "zones/foo", "newname": "zones/bar"}),
            "rename zones/foo zones/bar",
        ),
        (
            TaskName::ZfsSnapshotDataset,
            serde_json::json!({"dataset": "zones/bar@snap1"}),
            "snapshot zones/bar@snap1",
        ),
        (
            TaskName::ZfsRollbackDataset,
            serde_json::json!({"dataset": "zones/bar@snap1"}),
            "rollback -r zones/bar@snap1",
        ),
        (
            TaskName::ZfsCloneDataset,
            serde_json::json!({"snapshot": "zones/bar@snap1", "dataset": "zones/baz"}),
            "clone zones/bar@snap1 zones/baz",
        ),
        (
            TaskName::ZfsSetProperties,
            serde_json::json!({"dataset": "zones/baz", "properties": {"quota": "10G"}}),
            "set quota=10G zones/baz",
        ),
    ];

    for (task, params, expected) in &cases {
        let resp = client
            .post(format!("{url}/tasks"))
            .json(&TaskRequest {
                task: *task,
                params: params.clone(),
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
        assert_eq!(body, serde_json::json!({}));
        // Verify the most-recently logged argv line.
        let log = std::fs::read_to_string(&argv_log).expect("read argv log");
        let last_line = log.lines().last().expect("at least one line");
        assert_eq!(
            last_line, *expected,
            "task {task:?} invoked zfs with {last_line}, expected {expected}"
        );
    }

    server.close().await.expect("close");
}

#[tokio::test]
async fn set_properties_invokes_zfs_set_per_property() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let argv_log = tmp.path().join("argv.log");
    let zfs = argv_recording_script(tmp.path(), &argv_log);
    let zpool = write_script(tmp.path(), "zpool", "#!/bin/sh\n");

    let tool = Arc::new(ZfsTool::with_bins(zfs, zpool));
    let (server, url) = start_agent(registry_all(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::ZfsSetProperties,
            params: serde_json::json!({
                "dataset": "zones/foo",
                "properties": { "compression": "lz4", "quota": "5G" }
            }),
        })
        .send()
        .await
        .expect("dispatch");
    assert!(resp.status().is_success(), "status: {}", resp.status());

    let log = std::fs::read_to_string(&argv_log).expect("read argv log");
    let mut lines: Vec<&str> = log.lines().collect();
    lines.sort(); // order across HashMap iteration is nondeterministic
    assert_eq!(
        lines,
        vec!["set compression=lz4 zones/foo", "set quota=5G zones/foo"]
    );

    server.close().await.expect("close");
}

#[tokio::test]
async fn create_surfaces_stderr_on_failure() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let zfs = write_script(
        tmp.path(),
        "zfs",
        "#!/bin/sh\necho 'cannot create zones/foo: dataset already exists' 1>&2\nexit 1\n",
    );
    let zpool = write_script(tmp.path(), "zpool", "#!/bin/sh\n");

    let tool = Arc::new(ZfsTool::with_bins(zfs, zpool));
    let (server, url) = start_agent(registry_all(tool)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::ZfsCreateDataset,
            params: serde_json::json!({"dataset": "zones/foo"}),
        })
        .send()
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    let msg = body["message"].as_str().expect("message");
    assert!(
        msg.contains("failed to create ZFS dataset \"zones/foo\"")
            && msg.contains("already exists"),
        "unexpected message: {msg}"
    );

    server.close().await.expect("close");
}
