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
    // Note: Fixtures from public bugview don't include renderedFields since
    // bugview-service does its own ADF→HTML conversion. The expand parameter
    // still works - it just returns None when the fixture lacks the data.
    let issue_with_expand = progenitor_client
        .get_issue()
        .issue_id_or_key("TRITON-2520")
        .expand("renderedFields")
        .send()
        .await
        .expect("get issue with expand");

    assert_eq!(issue_with_expand.key, "TRITON-2520");

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

/// Test that non-public issues (FAKE-PRIVATE-*) are correctly filtered out
///
/// This test verifies that:
/// 1. Non-public issues are NOT returned in search results for "public" label
/// 2. Non-public issues exist in jira-stub-server (can be fetched directly)
/// 3. The filtering is done by bugview based on labels
#[tokio::test]
async fn test_non_public_issues_filtered() {
    let jira_fixtures_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../jira-stub-server/fixtures");

    let jira_context =
        Arc::new(jira_stub_server::StubContext::from_fixtures(&jira_fixtures_dir).unwrap());

    // Verify non-public fixtures are loaded
    let issue_keys = jira_context.issue_keys();
    assert!(
        issue_keys.iter().any(|k| k.starts_with("FAKE-PRIVATE")),
        "test fixtures should include FAKE-PRIVATE issues"
    );

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
    .to_logger("jira-stub-nonpublic-test")
    .expect("jira logger");

    let jira_server =
        match dropshot::HttpServerStarter::new(&jira_config, jira_api, jira_context, &jira_log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                eprintln!("skipping non-public filter test: {}", e);
                return;
            }
        };

    let jira_addr = jira_server.local_addr();
    let jira_base_url = format!("http://{}", jira_addr);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = jira_client::Client::new(&jira_base_url);

    // Verify FAKE-PRIVATE-1 exists in jira-stub (has "internal" label)
    let private_issue = client
        .get_issue()
        .issue_id_or_key("FAKE-PRIVATE-1")
        .send()
        .await
        .expect("FAKE-PRIVATE-1 should exist in jira-stub");
    assert_eq!(private_issue.key, "FAKE-PRIVATE-1");

    // Search for "public" label - should NOT include FAKE-PRIVATE issues
    let public_search = client
        .search_issues()
        .jql("labels IN (public)")
        .send()
        .await
        .expect("public search");

    let public_keys: Vec<&str> = public_search
        .issues
        .iter()
        .map(|i| i.key.as_str())
        .collect();

    assert!(
        !public_keys.iter().any(|k| k.starts_with("FAKE-PRIVATE")),
        "public search should NOT include FAKE-PRIVATE issues, got: {:?}",
        public_keys
    );

    // Search for "internal" label - should include FAKE-PRIVATE-1
    let internal_search = client
        .search_issues()
        .jql("labels IN (internal)")
        .send()
        .await
        .expect("internal search");

    let internal_keys: Vec<&str> = internal_search
        .issues
        .iter()
        .map(|i| i.key.as_str())
        .collect();

    assert!(
        internal_keys.contains(&"FAKE-PRIVATE-1"),
        "internal search should include FAKE-PRIVATE-1, got: {:?}",
        internal_keys
    );

    jira_server.close().await.expect("shutdown");
}
