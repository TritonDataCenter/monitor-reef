// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Integration tests for bugview-service using jira-stub-server
//!
//! These tests spin up the JIRA stub server and verify that the fixture data
//! is correctly served via the JIRA REST API. This validates that the stub
//! can be used for end-to-end testing of bugview-service.

use std::sync::Arc;
use std::time::Duration;

/// Integration test that verifies the JIRA stub server works correctly
/// with the progenitor-generated jira-client.
#[tokio::test]
async fn test_jira_stub_server_with_progenitor_client() {
    // ========================================================================
    // Step 1: Start the JIRA stub server
    // ========================================================================
    let jira_fixtures_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../jira-stub-server/fixtures");

    let jira_context =
        Arc::new(jira_stub_server::StubContext::from_fixtures(&jira_fixtures_dir).unwrap());

    let jira_api = jira_stub_server::api_description().expect("jira api description");

    let jira_config = dropshot::ConfigDropshot {
        bind_address: "127.0.0.1:0".parse().unwrap(),
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let jira_log = dropshot::ConfigLogging::StderrTerminal {
        level: dropshot::ConfigLoggingLevel::Warn,
    }
    .to_logger("jira-stub-test")
    .expect("jira logger");

    let jira_server =
        match dropshot::HttpServerStarter::new(&jira_config, jira_api, jira_context, &jira_log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                eprintln!(
                    "skipping integration test: failed to start jira stub: {}",
                    e
                );
                return;
            }
        };

    let jira_addr = jira_server.local_addr();
    let jira_base_url = format!("http://{}", jira_addr);

    // Give server a moment to be ready
    tokio::time::sleep(Duration::from_millis(50)).await;

    // ========================================================================
    // Step 2: Use the progenitor-generated client to talk to the stub
    // ========================================================================

    let progenitor_client = jira_client::Client::new(&jira_base_url);

    // Test: Search for issues with "public" label
    let search_result = progenitor_client
        .search_issues()
        .jql("labels IN (public)")
        .send()
        .await
        .expect("search via progenitor client");

    assert!(
        !search_result.issues.is_empty(),
        "should have public issues"
    );

    let keys: Vec<&str> = search_result
        .issues
        .iter()
        .map(|i| i.key.as_str())
        .collect();
    // These issues exist in the real fixture data fetched from JIRA
    assert!(keys.contains(&"TRITON-2520"), "should contain TRITON-2520");
    assert!(keys.contains(&"TRITON-1813"), "should contain TRITON-1813");

    // Test: Get a specific issue
    let issue = progenitor_client
        .get_issue()
        .issue_id_or_key("TRITON-2520")
        .send()
        .await
        .expect("get issue via progenitor client");

    assert_eq!(issue.key, "TRITON-2520");
    assert!(
        issue
            .fields
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("UUID")
    );

    // Test: Get issue with renderedFields expansion
    let issue_with_rendered = progenitor_client
        .get_issue()
        .issue_id_or_key("TRITON-2520")
        .expand("renderedFields")
        .send()
        .await
        .expect("get issue with expand");

    assert!(
        issue_with_rendered.rendered_fields.is_some(),
        "should have rendered fields when expanded"
    );

    // Test: Get remote links endpoint works (may return empty for this issue)
    let _links = progenitor_client
        .get_remote_links()
        .issue_id_or_key("TRITON-2520")
        .send()
        .await
        .expect("get remote links endpoint should work");

    // Test: Verify resolved issue has resolution
    let resolved_issue = progenitor_client
        .get_issue()
        .issue_id_or_key("TRITON-1813")
        .send()
        .await
        .expect("get resolved issue");

    let resolution = resolved_issue
        .fields
        .get("resolution")
        .expect("should have resolution field");
    assert!(
        resolution.get("name").is_some(),
        "resolved issue should have resolution.name"
    );

    // Cleanup
    jira_server.close().await.expect("shutdown jira stub");
}

/// Test that the stub server correctly filters by label
#[tokio::test]
async fn test_stub_jira_label_filtering() {
    let jira_fixtures_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../jira-stub-server/fixtures");

    let jira_context =
        Arc::new(jira_stub_server::StubContext::from_fixtures(&jira_fixtures_dir).unwrap());

    let jira_api = jira_stub_server::api_description().expect("jira api description");

    let jira_config = dropshot::ConfigDropshot {
        bind_address: "127.0.0.1:0".parse().unwrap(),
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let jira_log = dropshot::ConfigLogging::StderrTerminal {
        level: dropshot::ConfigLoggingLevel::Warn,
    }
    .to_logger("jira-stub-label-test")
    .expect("jira logger");

    let jira_server =
        match dropshot::HttpServerStarter::new(&jira_config, jira_api, jira_context, &jira_log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                eprintln!("skipping label filter test: {}", e);
                return;
            }
        };

    let jira_addr = jira_server.local_addr();
    let jira_base_url = format!("http://{}", jira_addr);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = jira_client::Client::new(&jira_base_url);

    // Search for "bhyve" label - only OS-6892 has it in the real fixture data
    let search_result = client
        .search_issues()
        .jql("labels IN (bhyve)")
        .send()
        .await
        .expect("bhyve search");

    // Only OS-6892 has the "bhyve" label in our fixtures
    assert_eq!(
        search_result.issues.len(),
        1,
        "only one issue should have bhyve label"
    );
    assert_eq!(search_result.issues[0].key, "OS-6892");

    jira_server.close().await.expect("shutdown");
}
