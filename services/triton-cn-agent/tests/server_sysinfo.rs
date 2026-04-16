// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end test for the `server_sysinfo` task.
//!
//! The task wraps `/usr/bin/sysinfo`, which doesn't exist on developer
//! laptops. This test writes a shell script that emits a known JSON blob,
//! points the task at it, dispatches the task through the real Dropshot
//! server, and asserts the response.

#![allow(clippy::expect_used)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use cn_agent_api::{TaskName, TaskRequest, cn_agent_api_mod, types::Uuid};
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServer, HttpServerStarter};
use triton_cn_agent::{
    AgentContext, AgentMetadata, api_impl::CnAgentApiImpl, registry::TaskRegistry,
    smartos::tasks::server_sysinfo::ServerSysinfoTask,
};

const FIXTURE_JSON: &str = r#"{
    "UUID": "11111111-2222-3333-4444-555555555555",
    "Hostname": "test-cn",
    "Boot Time": "1700000000",
    "Admin IP": "10.0.0.42",
    "Network Interfaces": {
        "vmx0": {
            "NIC Names": ["admin"],
            "ip4addr": "10.0.0.42"
        }
    }
}"#;

fn write_mock_sysinfo_script(dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("mock-sysinfo.sh");
    let body = format!(
        "#!/bin/sh\ncat <<'JSON'\n{json}\nJSON\n",
        json = FIXTURE_JSON
    );
    let mut f = std::fs::File::create(&script).expect("create script");
    f.write_all(body.as_bytes()).expect("write script");
    let mut perms = f.metadata().expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod");
    script
}

async fn start_agent_with_sysinfo(
    script: &std::path::Path,
) -> (HttpServer<Arc<AgentContext>>, String) {
    let registry = TaskRegistry::builder()
        .register(
            TaskName::ServerSysinfo,
            ServerSysinfoTask::with_binary(script.display().to_string()),
        )
        .build();

    let metadata = AgentMetadata {
        name: "cn-agent".to_string(),
        version: "test".to_string(),
        server_uuid: Uuid::nil(),
        backend: "smartos".to_string(),
    };
    let context = Arc::new(AgentContext::new(metadata, registry));
    let api = cn_agent_api_mod::api_description::<CnAgentApiImpl>().expect("api description");
    let config = ConfigDropshot {
        bind_address: "127.0.0.1:0".parse().expect("bind addr"),
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };
    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Warn,
    }
    .to_logger("cn-agent-sysinfo-test")
    .expect("logger");

    let server = HttpServerStarter::new(&config, api, context, &log)
        .expect("server starter")
        .start();
    let url = format!("http://{}", server.local_addr());
    (server, url)
}

#[tokio::test]
async fn server_sysinfo_task_returns_sysinfo_under_sysinfo_key() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let script = write_mock_sysinfo_script(tmp.path());

    let (server, url) = start_agent_with_sysinfo(&script).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{url}/tasks"))
        .json(&TaskRequest {
            task: TaskName::ServerSysinfo,
            params: serde_json::Value::Null,
        })
        .send()
        .await
        .expect("dispatch");

    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(
        body["sysinfo"]["UUID"].as_str(),
        Some("11111111-2222-3333-4444-555555555555")
    );
    assert_eq!(body["sysinfo"]["Hostname"].as_str(), Some("test-cn"));

    server.close().await.expect("close");
}
