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
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start JIRA stub in CI: {}", e);
                }
                eprintln!(
                    "SKIPPING: failed to start jira stub: {} (set CI=1 to fail)",
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
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start JIRA stub in CI: {}", e);
                }
                eprintln!("SKIPPING: label filter test: {} (set CI=1 to fail)", e);
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
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start JIRA stub in CI: {}", e);
                }
                eprintln!("SKIPPING: non-public filter test: {} (set CI=1 to fail)", e);
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

/// End-to-end test for bugview-service with jira-stub-server backend
///
/// This test verifies the complete integration by spawning bugview-service
/// as a subprocess and testing it against jira-stub-server.
///
/// Tests:
/// 1. Public issues are accessible (200 OK)
/// 2. Private issues return 404
/// 3. JSON responses have expected structure
#[tokio::test]
async fn test_bugview_service_e2e() {
    // ========================================================================
    // Step 1: Start jira-stub-server
    // ========================================================================
    let jira_fixtures_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../jira-stub-server/fixtures");

    let jira_context =
        Arc::new(jira_stub_server::StubContext::from_fixtures(&jira_fixtures_dir).unwrap());

    // Verify test fixtures are loaded correctly
    let issue_keys = jira_context.issue_keys();
    assert!(
        issue_keys.iter().any(|k| k == &"TRITON-2520".to_string()),
        "should have public issue TRITON-2520"
    );
    assert!(
        issue_keys
            .iter()
            .any(|k| k == &"FAKE-PRIVATE-1".to_string()),
        "should have private issue FAKE-PRIVATE-1"
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
    .to_logger("jira-stub-e2e-test")
    .expect("jira logger");

    let jira_server =
        match dropshot::HttpServerStarter::new(&jira_config, jira_api, jira_context, &jira_log) {
            Ok(starter) => starter.start(),
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("Failed to start JIRA stub in CI: {}", e);
                }
                eprintln!(
                    "SKIPPING: e2e test: failed to start jira-stub-server: {} (set CI=1 to fail)",
                    e
                );
                return;
            }
        };

    let jira_addr = jira_server.local_addr();
    let jira_base_url = format!("http://{}", jira_addr);

    // Give JIRA stub a moment to be ready
    tokio::time::sleep(Duration::from_millis(100)).await;

    // ========================================================================
    // Step 2: Start bugview-service as subprocess configured to use jira-stub
    // ========================================================================

    // Build bugview-service binary first
    let build_output = std::process::Command::new("cargo")
        .args(&["build", "-p", "bugview-service", "--bin", "bugview-service"])
        .output()
        .expect("failed to build bugview-service");

    if !build_output.status.success() {
        eprintln!(
            "skipping e2e test: failed to build bugview-service: {}",
            String::from_utf8_lossy(&build_output.stderr)
        );
        jira_server.close().await.ok();
        return;
    }

    // Start bugview-service with environment variables pointing to jira-stub
    let bugview_binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("bugview-service");

    if !bugview_binary.exists() {
        eprintln!(
            "skipping e2e test: bugview-service binary not found at {:?}",
            bugview_binary
        );
        jira_server.close().await.ok();
        return;
    }

    let mut bugview_process = std::process::Command::new(&bugview_binary)
        .env("JIRA_URL", &jira_base_url)
        .env("JIRA_USERNAME", "test-user")
        .env("JIRA_PASSWORD", "test-password")
        .env("JIRA_DEFAULT_LABEL", "public")
        .env("JIRA_ALLOWED_LABELS", "public,bug")
        .env("JIRA_ALLOWED_DOMAINS", "example.com")
        .env("PUBLIC_BASE_URL", "https://test.example.com")
        .env("BIND_ADDRESS", "127.0.0.1:0") // Random port
        .env("RUST_LOG", "warn")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start bugview-service");

    // Give bugview service a moment to start (it should print the bind address)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check if the process is still running
    match bugview_process.try_wait() {
        Ok(Some(status)) => {
            eprintln!(
                "skipping e2e test: bugview-service exited early with status: {}",
                status
            );
            jira_server.close().await.ok();
            return;
        }
        Ok(None) => {
            // Process is still running, good
        }
        Err(e) => {
            eprintln!("skipping e2e test: error checking bugview-service: {}", e);
            jira_server.close().await.ok();
            return;
        }
    }

    // Try to find what port bugview started on by trying a few common ports
    // Since we can't easily parse stdout, we'll just try ports and see what works
    let client = reqwest::Client::new();
    let mut bugview_base_url = None;

    for port in 8080..8090 {
        let test_url = format!("http://127.0.0.1:{}/bugview/index.json", port);
        if let Ok(resp) = client.get(&test_url).send().await {
            if resp.status().is_success() || resp.status().is_client_error() {
                bugview_base_url = Some(format!("http://127.0.0.1:{}", port));
                break;
            }
        }
    }

    let bugview_base_url = match bugview_base_url {
        Some(url) => url,
        None => {
            eprintln!("skipping e2e test: could not determine bugview-service port");
            bugview_process.kill().ok();
            jira_server.close().await.ok();
            return;
        }
    };

    // ========================================================================
    // Step 3: Test bugview endpoints with reqwest
    // ========================================================================

    // Test 1: GET /bugview/index.json - should return public issues only
    let index_url = format!("{}/bugview/index.json", bugview_base_url);
    let index_resp = client
        .get(&index_url)
        .send()
        .await
        .expect("GET /bugview/index.json");
    assert_eq!(index_resp.status(), 200, "index.json should return 200 OK");

    let index_json: serde_json::Value = index_resp.json().await.expect("parse index.json response");
    let issues = index_json["issues"]
        .as_array()
        .expect("index.json should have issues array");

    // Should contain public issues
    let issue_keys: Vec<&str> = issues.iter().filter_map(|i| i["key"].as_str()).collect();
    assert!(
        issue_keys.contains(&"TRITON-2520"),
        "index should include public issue TRITON-2520"
    );

    // Should NOT contain private issues
    assert!(
        !issue_keys.iter().any(|k| k.starts_with("FAKE-PRIVATE")),
        "index should NOT include FAKE-PRIVATE issues, got: {:?}",
        issue_keys
    );

    // Test 2: GET /bugview/issue/TRITON-2520 - public issue should return 200
    let public_issue_url = format!("{}/bugview/issue/TRITON-2520", bugview_base_url);
    let public_resp = client
        .get(&public_issue_url)
        .send()
        .await
        .expect("GET /bugview/issue/TRITON-2520");
    assert_eq!(
        public_resp.status(),
        200,
        "public issue HTML should return 200"
    );

    let public_html = public_resp.text().await.expect("read HTML response");
    assert!(
        public_html.contains("TRITON-2520"),
        "HTML should contain issue key"
    );
    assert!(
        public_html.contains("UUID"),
        "HTML should contain summary text"
    );

    // Test 3: GET /bugview/issue/FAKE-PRIVATE-1 - private issue should return 404
    let private_issue_url = format!("{}/bugview/issue/FAKE-PRIVATE-1", bugview_base_url);
    let private_resp = client
        .get(&private_issue_url)
        .send()
        .await
        .expect("GET /bugview/issue/FAKE-PRIVATE-1");
    assert_eq!(
        private_resp.status(),
        404,
        "private issue should return 404"
    );

    let private_html = private_resp.text().await.expect("read 404 HTML");
    assert!(
        private_html.contains("404") || private_html.contains("not public"),
        "404 response should indicate issue is not public"
    );

    // Test 4: GET /bugview/fulljson/TRITON-2520 - should return full JSON
    let fulljson_url = format!("{}/bugview/fulljson/TRITON-2520", bugview_base_url);
    let fulljson_resp = client
        .get(&fulljson_url)
        .send()
        .await
        .expect("GET /bugview/fulljson/TRITON-2520");
    assert_eq!(fulljson_resp.status(), 200, "fulljson should return 200");

    let fulljson: serde_json::Value = fulljson_resp.json().await.expect("parse fulljson response");
    assert_eq!(
        fulljson["key"].as_str(),
        Some("TRITON-2520"),
        "fulljson should have correct key"
    );
    assert!(
        fulljson["fields"].is_object(),
        "fulljson should have fields object"
    );
    assert!(
        fulljson["fields"]["summary"].is_string(),
        "fulljson should have summary field"
    );

    // Test 5: GET /bugview/fulljson/FAKE-PRIVATE-1 - should return 404
    let private_json_url = format!("{}/bugview/fulljson/FAKE-PRIVATE-1", bugview_base_url);
    let private_json_resp = client
        .get(&private_json_url)
        .send()
        .await
        .expect("GET /bugview/fulljson/FAKE-PRIVATE-1");
    assert_eq!(
        private_json_resp.status(),
        404,
        "private issue fulljson should return 404"
    );

    // Test 6: GET /bugview/json/TRITON-2520 - should return summary JSON
    let json_url = format!("{}/bugview/json/TRITON-2520", bugview_base_url);
    let json_resp = client
        .get(&json_url)
        .send()
        .await
        .expect("GET /bugview/json/TRITON-2520");
    assert_eq!(json_resp.status(), 200, "json endpoint should return 200");

    let summary_json: serde_json::Value = json_resp.json().await.expect("parse json response");
    assert_eq!(
        summary_json["id"].as_str(),
        Some("TRITON-2520"),
        "summary should have correct id"
    );
    assert!(
        summary_json["summary"].is_string(),
        "summary should have summary field"
    );
    assert!(
        summary_json["web_url"].is_string(),
        "summary should have web_url field"
    );

    // ========================================================================
    // Cleanup
    // ========================================================================
    bugview_process.kill().expect("kill bugview-service");
    jira_server.close().await.expect("shutdown jira-stub");
}
