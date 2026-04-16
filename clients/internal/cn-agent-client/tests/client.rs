// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Exercise the Progenitor-generated client against a live cn-agent instance.

#![allow(clippy::expect_used)]

use std::sync::Arc;

use cn_agent_api::cn_agent_api_mod;
use cn_agent_client::{
    Client,
    types::{TaskName, TaskRequest},
};
use dropshot::{ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter};
use triton_cn_agent::{
    AgentContext, AgentMetadata, api_impl::CnAgentApiImpl, tasks::common_registry,
};

async fn spawn_agent() -> (dropshot::HttpServer<Arc<AgentContext>>, String) {
    let metadata = AgentMetadata {
        name: "cn-agent".to_string(),
        version: "test".to_string(),
        server_uuid: uuid::Uuid::nil(),
        backend: "dummy".to_string(),
    };
    let context = Arc::new(AgentContext::new(metadata, common_registry()));
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
    .to_logger("cn-agent-client-test")
    .expect("logger");

    let server = HttpServerStarter::new(&config, api, context, &log)
        .expect("server starter")
        .start();
    let url = format!("http://{}", server.local_addr());
    (server, url)
}

#[tokio::test]
async fn progenitor_client_pings() {
    let (server, url) = spawn_agent().await;
    let client = Client::new(&url);

    let resp = client.ping().send().await.expect("ping");
    assert_eq!(resp.name, "cn-agent");
    assert_eq!(resp.backend, "dummy");
    assert!(!resp.paused);

    server.close().await.expect("close");
}

#[tokio::test]
async fn progenitor_client_dispatches_nop() {
    let (server, url) = spawn_agent().await;
    let client = Client::new(&url);

    let resp = client
        .dispatch_task()
        .body(TaskRequest {
            task: TaskName::Nop,
            params: None,
        })
        .send()
        .await
        .expect("dispatch");

    let body = resp.into_inner();
    assert_eq!(body, serde_json::json!({}));

    server.close().await.expect("close");
}

#[tokio::test]
async fn progenitor_client_roundtrips_history() {
    let (server, url) = spawn_agent().await;
    let client = Client::new(&url);

    client
        .dispatch_task()
        .body(TaskRequest {
            task: TaskName::Nop,
            params: None,
        })
        .send()
        .await
        .expect("dispatch");

    let history = client.get_history().send().await.expect("history");
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].task, TaskName::Nop);

    server.close().await.expect("close");
}

#[tokio::test]
async fn progenitor_client_pause_resume() {
    let (server, url) = spawn_agent().await;
    let client = Client::new(&url);

    client.pause().send().await.expect("pause");
    let ping = client.ping().send().await.expect("ping");
    assert!(ping.paused);

    client.resume().send().await.expect("resume");
    let ping = client.ping().send().await.expect("ping");
    assert!(!ping.paused);

    server.close().await.expect("close");
}
