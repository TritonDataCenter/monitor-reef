// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Phase 0 smoke test: spin up `tritond` on a random local port, hit
//! `/v2/health` via the generated client, verify the response.
//!
//! Verifies the OpenAPI-first toolchain end to end: the trait in
//! `tritond-api` produces a spec, the spec generates a client in
//! `tritond-client`, and that client speaks to a real `tritond`
//! Dropshot server in-process.

use tritond::{VERSION, start_server};

/// Spin up tritond on an ephemeral port, ask it for `/v2/health` via
/// the generated client, and assert the body shape.
#[tokio::test]
async fn health_endpoint_returns_ok_via_generated_client() {
    let server = start_server("127.0.0.1:0")
        .await
        .expect("server should start on ephemeral port");
    let bind = server.local_addr();

    let client = tritond_client::Client::new(&format!("http://{bind}"));

    let response = client
        .health()
        .send()
        .await
        .expect("health request should succeed");

    let body = response.into_inner();
    assert_eq!(body.status, "ok");
    assert_eq!(body.version, VERSION);

    // Drop the server explicitly to avoid leaking the listener between
    // tests when we add more.
    server.close().await.expect("server should close cleanly");
}
