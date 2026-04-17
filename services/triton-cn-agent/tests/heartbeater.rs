// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests for the CNAPI client + heartbeater.
//!
//! Stands up a miniature Dropshot "stub CNAPI" that records every incoming
//! request and returns 204, then runs the real CnapiClient / Heartbeater
//! against it so we exercise the exact wire format.

#![allow(clippy::expect_used)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use dropshot::{
    ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseUpdatedNoContent,
    HttpServer, HttpServerStarter, Path as DropshotPath, RequestContext, TypedBody, endpoint,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::time::timeout;

use triton_cn_agent::{
    cnapi::{AgentInfo, CnapiClient},
    heartbeater::{Heartbeater, status::StatusCollector},
    smartos::{VmadmTool, ZfsTool},
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Stub CNAPI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct CallLog {
    heartbeats: usize,
    statuses: Vec<serde_json::Value>,
    sysinfo: Vec<serde_json::Value>,
    agents: Vec<serde_json::Value>,
}

struct StubContext {
    calls: Mutex<CallLog>,
}

#[derive(Deserialize, JsonSchema)]
struct ServerPath {
    // Bound so Dropshot parses the UUID from the path; the handlers don't
    // inspect it further since they just want to count requests.
    #[allow(dead_code)]
    uuid: String,
}

#[endpoint {
    method = POST,
    path = "/servers/{uuid}/events/heartbeat",
}]
async fn heartbeat_handler(
    rqctx: RequestContext<Arc<StubContext>>,
    _path: DropshotPath<ServerPath>,
    _body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    rqctx.context().calls.lock().expect("lock").heartbeats += 1;
    Ok(HttpResponseUpdatedNoContent())
}

#[endpoint {
    method = POST,
    path = "/servers/{uuid}/events/status",
}]
async fn status_handler(
    rqctx: RequestContext<Arc<StubContext>>,
    _path: DropshotPath<ServerPath>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    rqctx
        .context()
        .calls
        .lock()
        .expect("lock")
        .statuses
        .push(body.into_inner());
    Ok(HttpResponseUpdatedNoContent())
}

#[endpoint {
    method = POST,
    path = "/servers/{uuid}/sysinfo",
}]
async fn sysinfo_handler(
    rqctx: RequestContext<Arc<StubContext>>,
    _path: DropshotPath<ServerPath>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    rqctx
        .context()
        .calls
        .lock()
        .expect("lock")
        .sysinfo
        .push(body.into_inner());
    Ok(HttpResponseUpdatedNoContent())
}

#[endpoint {
    method = POST,
    path = "/servers/{uuid}",
}]
async fn agents_handler(
    rqctx: RequestContext<Arc<StubContext>>,
    _path: DropshotPath<ServerPath>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    rqctx
        .context()
        .calls
        .lock()
        .expect("lock")
        .agents
        .push(body.into_inner());
    Ok(HttpResponseUpdatedNoContent())
}

async fn start_stub_cnapi() -> (HttpServer<Arc<StubContext>>, String, Arc<StubContext>) {
    let ctx = Arc::new(StubContext {
        calls: Mutex::new(CallLog::default()),
    });
    let mut api = dropshot::ApiDescription::new();
    api.register(heartbeat_handler).expect("register heartbeat");
    api.register(status_handler).expect("register status");
    api.register(sysinfo_handler).expect("register sysinfo");
    api.register(agents_handler).expect("register agents");

    let config = ConfigDropshot {
        bind_address: "127.0.0.1:0".parse().expect("bind addr"),
        default_request_body_max_bytes: 4 * 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };
    let log = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Warn,
    }
    .to_logger("stub-cnapi")
    .expect("logger");

    let server = HttpServerStarter::new(&config, api, ctx.clone(), &log)
        .expect("server starter")
        .start();
    let url = format!("http://{}", server.local_addr());
    (server, url, ctx)
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create script");
    f.write_all(body.as_bytes()).expect("write script");
    let mut perms = f.metadata().expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// Wait up to `timeout_secs` for `pred` to become true.
async fn wait_for(
    ctx: &StubContext,
    pred: impl Fn(&CallLog) -> bool,
    timeout_secs: u64,
) -> CallLog {
    let deadline = Duration::from_secs(timeout_secs);
    timeout(deadline, async {
        loop {
            {
                let guard = ctx.calls.lock().expect("lock");
                if pred(&guard) {
                    return guard.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("predicate timed out")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cnapi_client_posts_heartbeat_and_sysinfo() {
    let (server, url, ctx) = start_stub_cnapi().await;
    let server_uuid = Uuid::new_v4();
    let client = CnapiClient::new(&url, server_uuid).expect("client");

    client.post_heartbeat().await.expect("heartbeat");

    client
        .register_sysinfo(&serde_json::json!({"UUID": server_uuid.to_string()}))
        .await
        .expect("sysinfo");

    client
        .post_agents(&[AgentInfo {
            name: "net-agent".to_string(),
            image_uuid: "img-uuid".to_string(),
            uuid: Some("agent-uuid".to_string()),
            version: Some("2.2.0".to_string()),
        }])
        .await
        .expect("agents");

    let calls = ctx.calls.lock().expect("lock").clone();
    assert_eq!(calls.heartbeats, 1);
    assert_eq!(calls.sysinfo.len(), 1);
    assert_eq!(
        calls.sysinfo[0]["sysinfo"]["UUID"].as_str(),
        Some(server_uuid.to_string()).as_deref()
    );
    assert_eq!(calls.agents.len(), 1);
    assert_eq!(calls.agents[0]["agents"][0]["name"], "net-agent");
    assert_eq!(calls.agents[0]["agents"][0]["version"], "2.2.0");

    server.close().await.expect("close");
}

#[tokio::test]
async fn heartbeater_ticks_and_posts_status() {
    let (server, url, ctx) = start_stub_cnapi().await;

    let tmp = tempfile::tempdir().expect("tmpdir");
    // Mock vmadm returns an empty lookup array. (Heartbeater calls it
    // once per status collection.)
    let vmadm = write_script(tmp.path(), "vmadm", "#!/bin/sh\necho '[]'\n");
    // Mock zpool list returns one pool row.
    let zpool = write_script(
        tmp.path(),
        "zpool",
        // name size allocated free cap health altroot
        "#!/bin/sh\n\
         printf 'zones\\t960197124096\\t12884901888\\t947312222208\\t1\\tONLINE\\t-\\n'\n",
    );
    let zfs = write_script(tmp.path(), "zfs", "#!/bin/sh\n");

    let vmadm_tool = Arc::new(VmadmTool::with_bin(vmadm));
    let zfs_tool = Arc::new(ZfsTool::with_bins(zfs, zpool));

    let server_uuid = Uuid::new_v4();
    let cnapi = Arc::new(CnapiClient::new(&url, server_uuid).expect("client"));
    let collector = StatusCollector::new(vmadm_tool, zfs_tool);

    let heartbeater = Heartbeater::new(cnapi, collector)
        .with_heartbeat_interval(Duration::from_millis(50))
        .with_status_check_interval(Duration::from_millis(200))
        .with_status_max_interval(Duration::from_millis(100));

    let handle = heartbeater.spawn();

    // Wait for at least one heartbeat + one status post to land.
    let calls = wait_for(&ctx, |c| c.heartbeats >= 1 && !c.statuses.is_empty(), 5).await;
    assert!(calls.heartbeats >= 1);
    assert!(!calls.statuses.is_empty());
    let status = &calls.statuses[0];
    assert_eq!(
        status["zpoolStatus"]["zones"]["bytes_used"],
        12_884_901_888_i64
    );
    assert_eq!(
        status["zpoolStatus"]["zones"]["bytes_available"],
        947_312_222_208_i64
    );
    assert!(status["timestamp"].is_string());

    handle.shutdown().await;
    server.close().await.expect("close");
}

#[tokio::test]
async fn heartbeater_survives_cnapi_errors() {
    // Stub CNAPI is never started — so every CnapiClient call fails. Ensure
    // the loop keeps running and responds to shutdown.
    let server_uuid = Uuid::new_v4();
    let cnapi = Arc::new(
        CnapiClient::builder("http://127.0.0.1:1", server_uuid)
            .with_connect_timeout(Duration::from_millis(50))
            .with_request_timeout(Duration::from_millis(50))
            .build()
            .expect("client"),
    );
    // Never-called tools; the test just verifies the loop survives failing
    // CNAPI posts.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let vmadm = write_script(tmp.path(), "vmadm", "#!/bin/sh\necho '[]'\n");
    let zpool = write_script(tmp.path(), "zpool", "#!/bin/sh\n");
    let zfs = write_script(tmp.path(), "zfs", "#!/bin/sh\n");
    let collector = StatusCollector::new(
        Arc::new(VmadmTool::with_bin(vmadm)),
        Arc::new(ZfsTool::with_bins(zfs, zpool)),
    );

    let heartbeater = Heartbeater::new(cnapi, collector)
        .with_heartbeat_interval(Duration::from_millis(50))
        .with_status_check_interval(Duration::from_millis(200))
        .with_status_max_interval(Duration::from_millis(100));

    let handle = heartbeater.spawn();
    // Let it iterate a bit through failures.
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.shutdown().await;
}
