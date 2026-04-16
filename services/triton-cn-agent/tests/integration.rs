// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests against the real Dropshot server.
//!
//! Each test spins up an ephemeral server on a random port, exercises the
//! HTTP endpoints with `reqwest`, and shuts down.

// Integration tests may use expect() freely — the workspace clippy config
// opts this crate into `allow-expect-in-tests`, but that only applies to
// expressions inside `#[test]` functions. The shared helpers below live
// outside of those, so whitelist explicitly.
#![allow(clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cn_agent_api::{
    PingResponse, TaskHistoryResponse, TaskName, TaskRequest, TaskStatus, cn_agent_api_mod,
};
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServer, HttpServerStarter};
use triton_cn_agent::{
    AgentContext, AgentMetadata, api_impl::CnAgentApiImpl, tasks::common_registry,
};

async fn start_test_server() -> (HttpServer<Arc<AgentContext>>, SocketAddr) {
    let metadata = AgentMetadata {
        name: "cn-agent".to_string(),
        version: "test".to_string(),
        server_uuid: uuid::Uuid::nil(),
        backend: "dummy".to_string(),
    };
    let context = Arc::new(AgentContext::new(metadata, common_registry()));

    let api = cn_agent_api_mod::api_description::<CnAgentApiImpl>().expect("api description");

    let config = ConfigDropshot {
        bind_address: "127.0.0.1:0".parse().expect("parse bind addr"),
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };
    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Warn,
    }
    .to_logger("cn-agent-test")
    .expect("build logger");

    let server = HttpServerStarter::new(&config, api, context, &log)
        .expect("create server")
        .start();
    let addr = server.local_addr();
    (server, addr)
}

#[tokio::test]
async fn ping_returns_agent_metadata() {
    let (server, addr) = start_test_server().await;

    let resp: PingResponse = reqwest::get(format!("http://{addr}/ping"))
        .await
        .expect("ping request")
        .json()
        .await
        .expect("ping json");

    assert_eq!(resp.name, "cn-agent");
    assert_eq!(resp.backend, "dummy");
    assert!(!resp.paused);

    server.close().await.expect("close");
}

#[tokio::test]
async fn dispatch_nop_task_succeeds() {
    let (server, addr) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/tasks"))
        .json(&TaskRequest {
            task: TaskName::Nop,
            params: serde_json::Value::Null,
        })
        .send()
        .await
        .expect("dispatch nop");

    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body, serde_json::json!({}));

    server.close().await.expect("close");
}

#[tokio::test]
async fn dispatch_sleep_task_with_error_returns_500() {
    let (server, addr) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/tasks"))
        .json(&TaskRequest {
            task: TaskName::Sleep,
            params: serde_json::json!({"error": "boom"}),
        })
        .send()
        .await
        .expect("dispatch sleep");

    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["message"], "boom");

    server.close().await.expect("close");
}

#[tokio::test]
async fn dispatch_sleep_task_waits_then_succeeds() {
    let (server, addr) = start_test_server().await;
    let client = reqwest::Client::new();

    let start = std::time::Instant::now();
    let resp = client
        .post(format!("http://{addr}/tasks"))
        .json(&TaskRequest {
            task: TaskName::Sleep,
            params: serde_json::json!({"sleep": 1}),
        })
        .send()
        .await
        .expect("dispatch sleep");

    assert!(resp.status().is_success());
    assert!(
        start.elapsed() >= Duration::from_millis(900),
        "sleep should take at least ~1s, took {:?}",
        start.elapsed()
    );

    server.close().await.expect("close");
}

#[tokio::test]
async fn pause_blocks_task_dispatch() {
    let (server, addr) = start_test_server().await;
    let client = reqwest::Client::new();

    let pause = client
        .post(format!("http://{addr}/pause"))
        .send()
        .await
        .expect("pause");
    assert_eq!(pause.status(), 204);

    let resp = client
        .post(format!("http://{addr}/tasks"))
        .json(&TaskRequest {
            task: TaskName::Nop,
            params: serde_json::Value::Null,
        })
        .send()
        .await
        .expect("dispatch while paused");
    assert_eq!(resp.status(), 503);

    // Resume clears the flag.
    client
        .post(format!("http://{addr}/resume"))
        .send()
        .await
        .expect("resume");
    let resp = client
        .post(format!("http://{addr}/tasks"))
        .json(&TaskRequest {
            task: TaskName::Nop,
            params: serde_json::Value::Null,
        })
        .send()
        .await
        .expect("dispatch after resume");
    assert!(resp.status().is_success());

    server.close().await.expect("close");
}

#[tokio::test]
async fn unregistered_task_returns_404() {
    let (server, addr) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/tasks"))
        .json(&TaskRequest {
            // Dummy backend doesn't register machine_load.
            task: TaskName::MachineLoad,
            params: serde_json::json!({"uuid": "00000000-0000-0000-0000-000000000000"}),
        })
        .send()
        .await
        .expect("dispatch unregistered");

    assert_eq!(resp.status(), 404);

    server.close().await.expect("close");
}

#[tokio::test]
async fn history_records_dispatched_tasks() {
    let (server, addr) = start_test_server().await;
    let client = reqwest::Client::new();

    for _ in 0..3 {
        client
            .post(format!("http://{addr}/tasks"))
            .json(&TaskRequest {
                task: TaskName::Nop,
                params: serde_json::Value::Null,
            })
            .send()
            .await
            .expect("dispatch");
    }

    let history: TaskHistoryResponse = client
        .get(format!("http://{addr}/history"))
        .send()
        .await
        .expect("history")
        .json()
        .await
        .expect("history json");

    assert_eq!(history.entries.len(), 3);
    for entry in &history.entries {
        assert_eq!(entry.task, TaskName::Nop);
        assert_eq!(entry.status, TaskStatus::Finished);
        assert!(entry.finished_at.is_some());
    }

    server.close().await.expect("close");
}
