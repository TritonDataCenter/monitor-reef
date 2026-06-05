// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end test for `tritonadm bootstrap`: bring up `tritond` on an
//! ephemeral port in-process, run the `tritonadm` binary as a subprocess
//! pointed at it, and assert that it exits successfully and reports
//! the expected status/version.

use assert_cmd::Command;
use predicates::str::contains;
use tritond::{VERSION, start_server};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_against_running_tritond() {
    let server = start_server("127.0.0.1:0")
        .await
        .expect("server should start on ephemeral port");
    let bind = server.local_addr();
    let endpoint = format!("http://{bind}");

    // Run the tritonadm binary as a subprocess; assert_cmd compiles it
    // via the package's binary target.
    let mut cmd = Command::cargo_bin("tritonadm").expect("tritonadm binary should exist");
    cmd.args(["bootstrap", "--endpoint", &endpoint]);

    cmd.assert()
        .success()
        .stdout(contains("status:  ok"))
        .stdout(contains(format!("version: {VERSION}")));

    server.close().await.expect("server should close cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_json_output_is_valid_json() {
    let server = start_server("127.0.0.1:0")
        .await
        .expect("server should start on ephemeral port");
    let bind = server.local_addr();
    let endpoint = format!("http://{bind}");

    let mut cmd = Command::cargo_bin("tritonadm").expect("tritonadm binary should exist");
    cmd.args(["bootstrap", "--endpoint", &endpoint, "--json"]);

    let output = cmd.output().expect("tritonadm should run");
    assert!(
        output.status.success(),
        "tritonadm exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["version"], VERSION);
    assert_eq!(parsed["endpoint"], endpoint);

    server.close().await.expect("server should close cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_fails_when_tritond_is_unreachable() {
    // Bind a listener and immediately drop it so the port is free.
    // This gives us a port that almost certainly has nothing listening.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let dead_addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let mut cmd = Command::cargo_bin("tritonadm").expect("tritonadm binary should exist");
    cmd.args(["bootstrap", "--endpoint", &format!("http://{dead_addr}")]);

    cmd.assert()
        .failure()
        .stderr(contains("failed to reach tritond"));
}
