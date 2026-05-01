// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! End-to-end tests for the `/v2/silos` surface using an in-memory
//! [`tritond_store::MemStore`] behind the running Dropshot server.

use tritond::start_server;
use tritond_client::types::NewSilo;

/// POST a silo, then GET it back by id; the round-tripped record must
/// match what we sent (modulo server-assigned id and timestamp).
#[tokio::test]
async fn create_then_get_round_trips_via_generated_client() {
    let server = start_server("127.0.0.1:0")
        .await
        .expect("server should start on ephemeral port");
    let bind = server.local_addr();
    let client = tritond_client::Client::new(&format!("http://{bind}"));

    let created = client
        .create_silo()
        .body(NewSilo {
            name: "operator".to_string(),
            description: Some("the bootstrap silo".to_string()),
        })
        .send()
        .await
        .expect("create_silo should succeed")
        .into_inner();

    assert_eq!(created.name, "operator");
    assert_eq!(created.description, "the bootstrap silo");

    let fetched = client
        .get_silo()
        .silo_id(created.id)
        .send()
        .await
        .expect("get_silo should succeed")
        .into_inner();

    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, "operator");
    assert_eq!(fetched.description, "the bootstrap silo");
    assert_eq!(fetched.created_at, created.created_at);

    server.close().await.expect("server should close cleanly");
}

/// Creating a silo whose name is already taken must fail with 409.
#[tokio::test]
async fn duplicate_name_returns_409() {
    let server = start_server("127.0.0.1:0")
        .await
        .expect("server should start on ephemeral port");
    let bind = server.local_addr();
    let client = tritond_client::Client::new(&format!("http://{bind}"));

    client
        .create_silo()
        .body(NewSilo {
            name: "ops".to_string(),
            description: None,
        })
        .send()
        .await
        .expect("first create should succeed");

    let err = client
        .create_silo()
        .body(NewSilo {
            name: "ops".to_string(),
            description: None,
        })
        .send()
        .await
        .expect_err("second create should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 409);

    server.close().await.expect("server should close cleanly");
}

/// Looking up an unknown silo id must yield 404.
#[tokio::test]
async fn missing_silo_returns_404() {
    let server = start_server("127.0.0.1:0")
        .await
        .expect("server should start on ephemeral port");
    let bind = server.local_addr();
    let client = tritond_client::Client::new(&format!("http://{bind}"));

    let err = client
        .get_silo()
        .silo_id(uuid::Uuid::new_v4())
        .send()
        .await
        .expect_err("get_silo on unknown id should fail");

    let progenitor_client::Error::ErrorResponse(response) = err else {
        panic!("expected ErrorResponse, got {err:?}");
    };
    assert_eq!(response.status().as_u16(), 404);

    server.close().await.expect("server should close cleanly");
}
