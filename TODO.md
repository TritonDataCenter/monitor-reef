# Bugview PR Action Items

This document tracks the action items identified during the PR review of the `bugview` branch.

## Must Fix Before Merge

### 1. Add End-to-End Integration Test

**Priority:** Critical
**Files:**
- `services/bugview-service/tests/integration_test.rs`

**Problem:** The current integration test only tests jira-stub-server with jira-client. It does NOT test bugview-service connected to jira-stub-server, despite the file's docstring claiming otherwise.

**Implementation Steps:**

1. Add a new test function `test_bugview_service_e2e` after line 312
2. Start jira-stub-server on a random port
3. Start bugview-service configured to use jira-stub-server as its JIRA backend
4. Use reqwest to hit bugview endpoints and verify:
   - `GET /bugview/index.json` returns public issues only (TRITON-2520, TRITON-1813, OS-6892)
   - `GET /bugview/issue/TRITON-2520` returns HTML with issue content
   - `GET /bugview/issue/FAKE-PRIVATE-1` returns 404 (private issue filtered)
   - `GET /bugview/fulljson/TRITON-2520` returns JSON with expected fields
   - Pagination works across requests

**Test Skeleton:**
```rust
#[tokio::test]
async fn test_bugview_service_e2e() {
    // 1. Start jira-stub-server
    let jira_server = start_jira_stub_server().await;
    let jira_url = format!("http://{}", jira_server.local_addr());

    // 2. Start bugview-service pointing to jira-stub
    let bugview_server = start_bugview_service(&jira_url).await;
    let bugview_url = format!("http://{}", bugview_server.local_addr());

    // 3. Test public issue access
    let client = reqwest::Client::new();
    let resp = client.get(format!("{}/bugview/issue/TRITON-2520", bugview_url))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // 4. Test private issue returns 404
    let resp = client.get(format!("{}/bugview/issue/FAKE-PRIVATE-1", bugview_url))
        .send().await.unwrap();
    assert_eq!(resp.status(), 404);

    // 5. Test index.json excludes private issues
    let resp = client.get(format!("{}/bugview/index.json", bugview_url))
        .send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let issues = body["issues"].as_array().unwrap();
    assert!(issues.iter().all(|i| !i["key"].as_str().unwrap().starts_with("FAKE-PRIVATE")));
}
```

**Acceptance Criteria:**
- [ ] Test starts both servers successfully
- [ ] Test verifies public issues are accessible
- [ ] Test verifies private issues return 404
- [ ] Test verifies index excludes private issues
- [ ] Test runs in CI (or explicitly skips with `#[ignore]` annotation)

---

### 2. Add Test for Non-Public Issue Access Control

**Priority:** Critical (Security)
**Files:**
- `services/bugview-service/src/main.rs` (add after line 943)

**Problem:** The mock JIRA client (`MockJiraClient`) always returns issues with the "public" label. No test verifies that issues WITHOUT the public label correctly return 404.

**Implementation Steps:**

1. Create a new mock that returns issues without the "public" label
2. Start a test server with this mock
3. Verify all issue endpoints return 404 for non-public issues

**Test Code:**
```rust
/// Test that non-public issues return 404 on all endpoints
#[tokio::test]
async fn test_http_nonpublic_issue_returns_404() {
    struct NonPublicMockJiraClient;

    #[async_trait::async_trait]
    impl JiraClientTrait for NonPublicMockJiraClient {
        async fn get_issue(&self, _key: &str) -> anyhow::Result<Issue> {
            let mut fields = serde_json::Map::new();
            fields.insert("summary".into(), json!("Secret Issue"));
            fields.insert("labels".into(), json!(["internal", "private"])); // NO "public" label
            fields.insert("status".into(), json!({"name": "Open"}));
            Ok(Issue {
                key: "SECRET-1".into(),
                id: "99999".into(),
                fields,
                rendered_fields: None,
            })
        }

        async fn search_issues(&self, _labels: &[String], _token: Option<&str>, _sort: &str)
            -> anyhow::Result<SearchResponse> {
            Ok(SearchResponse { issues: vec![], is_last: Some(true), next_page_token: None })
        }

        async fn get_remote_links(&self, _id: &str) -> anyhow::Result<Vec<RemoteLink>> {
            Ok(vec![])
        }
    }

    // Start server with NonPublicMockJiraClient
    let ctx = ApiContext { /* ... with NonPublicMockJiraClient ... */ };
    let server = start_test_server(ctx).await;
    let client = reqwest::Client::new();
    let base = format!("http://{}", server.local_addr());

    // All issue endpoints should return 404
    for endpoint in &["/bugview/issue/SECRET-1", "/bugview/json/SECRET-1", "/bugview/fulljson/SECRET-1"] {
        let resp = client.get(format!("{}{}", base, endpoint)).send().await.unwrap();
        assert_eq!(resp.status(), 404, "Endpoint {} should return 404 for non-public issue", endpoint);
    }
}
```

**Acceptance Criteria:**
- [ ] Test creates mock that returns issue without "public" label
- [ ] Test verifies `/bugview/issue/{key}` returns 404
- [ ] Test verifies `/bugview/json/{key}` returns 404
- [ ] Test verifies `/bugview/fulljson/{key}` returns 404

---

### 3. Add Logging to Silent `unwrap_or_default()` Calls

**Priority:** Critical
**Files:**
- `services/bugview-service/src/main.rs:248-252`
- `services/bugview-service/src/main.rs:403-408`

**Problem:** When fetching remote links fails, errors are silently swallowed and empty vectors returned. Users see missing links with no indication of failure.

**Implementation Steps:**

1. Replace `.unwrap_or_default()` with `.unwrap_or_else()` that logs
2. Add appropriate log level (warn) and context

**Before (line 248-252):**
```rust
let jira_remote_links = ctx
    .jira
    .get_remote_links(&issue.id)
    .await
    .unwrap_or_default();
```

**After:**
```rust
let jira_remote_links = ctx
    .jira
    .get_remote_links(&issue.id)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(
            issue_id = %issue.id,
            issue_key = %issue.key,
            error = %e,
            "Failed to fetch remote links, returning empty list"
        );
        Vec::new()
    });
```

**Same change for lines 403-408.**

**Acceptance Criteria:**
- [ ] Both locations log errors with issue context
- [ ] Log level is `warn` (not error, since service continues)
- [ ] Build passes with no warnings

---

## Should Fix Soon

### 4. Add Test for Domain Filtering Blocking Disallowed Domains

**Priority:** High (Security)
**Files:**
- `services/bugview-service/src/main.rs` (add after line 857)

**Problem:** `filter_remote_links()` at lines 581-599 filters links by allowed domains, but tests only verify allowed domains pass through. No test verifies disallowed domains are blocked.

**Implementation Steps:**

1. Add test with mixed allowed/disallowed domain links
2. Verify only allowed domain links remain after filtering

**Test Code:**
```rust
#[tokio::test]
async fn test_remote_link_domain_filter_blocks_disallowed() {
    let config = Config {
        default_label: "public".into(),
        allowed_labels: vec![],
        allowed_domains: vec!["safe.example.com".into()],
        public_base_url: "https://test.example.com".into(),
    };

    let links: Vec<jira_client::RemoteLink> = serde_json::from_value(json!([
        {"id": 1, "object": {"url": "https://safe.example.com/good", "title": "Good Link"}},
        {"id": 2, "object": {"url": "https://manta.joyent.us/signed/secret", "title": "Signed URL"}},
        {"id": 3, "object": {"url": "https://internal.corp/secret", "title": "Internal"}},
        {"id": 4, "object": {"url": "https://safe.example.com/another", "title": "Also Good"}},
    ])).unwrap();

    let filtered = filter_remote_links(&links, &config);

    assert_eq!(filtered.len(), 2, "Should only keep links from allowed domain");
    assert!(filtered.iter().all(|l| l.url.contains("safe.example.com")));
}
```

**Acceptance Criteria:**
- [ ] Test includes links from both allowed and disallowed domains
- [ ] Test verifies only allowed domain links remain
- [ ] Test verifies count matches expected

---

### 5. Fix Tests to Fail in CI When Server Setup Fails

**Priority:** High
**Files:**
- `services/bugview-service/src/main.rs:895-900, 932-935, 962-965, 991-994, 1018-1021`
- `services/bugview-service/tests/integration_test.rs:47-54, 176-180, 246-253`

**Problem:** Tests silently return (pass) when server fails to start. This masks infrastructure issues in CI.

**Implementation Steps:**

1. Check for `CI` environment variable
2. Panic in CI environment, skip locally with clear message
3. Or use `#[ignore]` attribute with documentation

**Option A - Fail in CI:**
```rust
let server = match HttpServerStarter::new(&config_dropshot, api, ctx, &log) {
    Ok(starter) => starter.start(),
    Err(e) => {
        if std::env::var("CI").is_ok() {
            panic!("Failed to start test server in CI: {}", e);
        }
        eprintln!("SKIPPING: failed to start server: {} (run with CI=1 to fail)", e);
        return;
    }
};
```

**Option B - Use `#[ignore]` with custom runner:**
```rust
#[tokio::test]
#[ignore = "requires socket binding - run with --ignored in environments that support it"]
async fn test_http_issue_route_with_mock_server() {
    // Remove the early return, let it panic if server fails
    let server = HttpServerStarter::new(&config_dropshot, api, ctx, &log)
        .expect("Failed to start test server")
        .start();
    // ...
}
```

**Acceptance Criteria:**
- [ ] Tests fail loudly in CI when server setup fails
- [ ] Clear skip message when running locally without socket support
- [ ] Pattern applied consistently across all affected tests

---

### 6. Address ADF Code Duplication

**Priority:** High
**Files:**
- `cli/bugview-cli/src/main.rs:69-253` (`extract_adf_text`)
- `services/bugview-service/src/html.rs:298-479` (`adf_to_html`)

**Problem:** Both files contain nearly identical ADF parsing logic with similar node type handling. CLAUDE.md states: "YOU MUST WORK HARD to reduce code duplication."

**Implementation Steps:**

1. Create a new crate or module for shared ADF handling
2. Define a visitor/writer trait for output format abstraction
3. Implement HTML and text writers
4. Update CLI and service to use shared code

**Proposed Structure:**
```
apis/bugview-api/src/
├── lib.rs           # existing API types
└── adf.rs           # NEW: shared ADF handling

# Or create a new crate:
libs/adf-renderer/
├── Cargo.toml
└── src/
    ├── lib.rs       # AdfRenderer trait + types
    ├── html.rs      # HtmlRenderer impl
    └── text.rs      # TextRenderer impl (for CLI)
```

**Trait Design:**
```rust
pub trait AdfWriter {
    fn write_text(&mut self, text: &str);
    fn write_link(&mut self, url: &str, text: &str);
    fn start_paragraph(&mut self);
    fn end_paragraph(&mut self);
    fn start_bullet_list(&mut self);
    fn end_bullet_list(&mut self);
    // ... etc
}

pub fn render_adf<W: AdfWriter>(content: &[serde_json::Value], writer: &mut W) {
    // Shared traversal logic
}
```

**Acceptance Criteria:**
- [ ] ADF parsing logic exists in one place only
- [ ] CLI uses shared code with text output
- [ ] Service uses shared code with HTML output
- [ ] All existing tests pass
- [ ] No behavior changes

---

### 7. Update README to Reflect Actual Implementation

**Priority:** Medium
**Files:**
- `services/bugview-service/README.md:18, 198-199`

**Problem:** README claims the service uses "JIRA's `renderedFields` API" but the code actually does manual ADF-to-HTML conversion.

**Implementation Steps:**

1. Update line 18 from:
   > JIRA markup rendering - Full support via JIRA's `renderedFields` API

   To:
   > JIRA markup rendering - Full support via ADF (Atlassian Document Format) to HTML conversion

2. Update lines 198-199 to remove mention of `expand=renderedFields` if not actually used

3. Review other README claims for accuracy

**Acceptance Criteria:**
- [ ] README accurately describes ADF conversion approach
- [ ] No claims about renderedFields API usage
- [ ] Technical descriptions match actual implementation

---

## Consider Later

### 8. Create Newtypes for IssueKey, IssueId, PageToken

**Priority:** Medium
**Files:**
- `apis/jira-api/src/lib.rs` (define types)
- `apis/bugview-api/src/lib.rs` (use types)
- `services/bugview-service/src/jira_client.rs` (use types)
- `services/bugview-service/src/main.rs` (use types)

**Problem:** Issue keys, IDs, and pagination tokens are all raw `String` types. This allows mixing them up and provides no compile-time safety.

**Implementation Steps:**

1. Define newtypes with validation:
```rust
// apis/jira-api/src/lib.rs
use std::fmt;

/// A JIRA issue key in PROJECT-123 format
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct IssueKey(String);

impl IssueKey {
    pub fn new(key: impl Into<String>) -> Result<Self, InvalidIssueKey> {
        let key = key.into();
        if key.contains('-') && key.chars().any(|c| c.is_ascii_digit()) {
            Ok(Self(key))
        } else {
            Err(InvalidIssueKey(key))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A JIRA issue ID (numeric string)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct IssueId(String);

impl IssueId {
    pub fn new(id: impl Into<String>) -> Result<Self, InvalidIssueId> {
        let id = id.into();
        if id.chars().all(|c| c.is_ascii_digit()) {
            Ok(Self(id))
        } else {
            Err(InvalidIssueId(id))
        }
    }
}

/// An opaque pagination token
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PageToken(String);
```

2. Update trait signatures:
```rust
// jira_client.rs
async fn get_issue(&self, key: &IssueKey) -> Result<Issue>;
async fn get_remote_links(&self, id: &IssueId) -> Result<Vec<RemoteLink>>;
```

3. Update all call sites

**Acceptance Criteria:**
- [ ] `IssueKey` validates PROJECT-123 format
- [ ] `IssueId` validates numeric format
- [ ] Compile-time prevention of key/id confusion
- [ ] All tests pass

---

### 9. Add ADF Edge Case Tests

**Priority:** Low
**Files:**
- `services/bugview-service/src/html.rs` (add after line 541)

**Problem:** `adf_to_html()` handles many node types but tests only cover basic cases. Missing tests for XSS prevention, empty input, malformed ADF.

**Implementation Steps:**

Add these tests:

```rust
#[test]
fn adf_to_html_escapes_xss_attempts() {
    let input = json!([{
        "type": "text",
        "text": "<script>alert('xss')</script>"
    }]);
    let html = adf_to_html(&input);
    assert!(!html.contains("<script>"), "Script tags should be escaped");
    assert!(html.contains("&lt;script&gt;"), "Should contain escaped script tag");
}

#[test]
fn adf_to_html_handles_empty_array() {
    let input = json!([]);
    let html = adf_to_html(&input);
    assert_eq!(html, "");
}

#[test]
fn adf_to_html_handles_null_content() {
    let input = json!([{
        "type": "paragraph",
        "content": null
    }]);
    let html = adf_to_html(&input);
    assert!(html.contains("<p>") && html.contains("</p>"));
}

#[test]
fn adf_to_html_handles_missing_type() {
    let input = json!([{
        "text": "no type field"
    }]);
    let html = adf_to_html(&input);
    // Should not panic, should handle gracefully
    assert!(html.is_empty() || html.len() >= 0);
}

#[test]
fn adf_to_html_handles_deeply_nested_lists() {
    let input = json!([{
        "type": "bulletList",
        "content": [{
            "type": "listItem",
            "content": [{
                "type": "bulletList",
                "content": [{
                    "type": "listItem",
                    "content": [{
                        "type": "paragraph",
                        "content": [{"type": "text", "text": "deeply nested"}]
                    }]
                }]
            }]
        }]
    }]);
    let html = adf_to_html(&input);
    assert!(html.contains("deeply nested"));
    assert_eq!(html.matches("<ul>").count(), 2);
}

#[test]
fn adf_to_html_escapes_attribute_injection() {
    let input = json!([{
        "type": "inlineCard",
        "attrs": {
            "url": "javascript:alert('xss')"
        }
    }]);
    let html = adf_to_html(&input);
    // URL should be escaped or rejected
    assert!(!html.contains("javascript:"));
}
```

**Acceptance Criteria:**
- [ ] XSS via text content prevented
- [ ] XSS via attributes prevented
- [ ] Empty input handled gracefully
- [ ] Malformed ADF doesn't panic
- [ ] Nested structures work correctly

---

### 10. Improve Mutex Poisoning Handling

**Priority:** Low
**Files:**
- `services/bugview-service/src/main.rs:106, 147, 866`

**Problem:** `.lock().unwrap()` on mutex will panic if the mutex is poisoned (a thread panicked while holding it). While rare, this could cascade failures.

**Implementation Steps:**

Replace:
```rust
let mut cache = self.cache.lock().unwrap();
```

With:
```rust
let mut cache = self.cache.lock().unwrap_or_else(|poisoned| {
    tracing::error!("Token cache mutex was poisoned, recovering");
    poisoned.into_inner()
});
```

**Alternative:** If poisoning indicates unrecoverable state, keep the panic but add a descriptive message:
```rust
let mut cache = self.cache.lock().expect("token cache mutex poisoned - this indicates a prior panic");
```

**Acceptance Criteria:**
- [ ] Mutex locking handles poisoning gracefully
- [ ] Clear logging when poisoning occurs
- [ ] Service continues operating after mutex recovery

---

## Progress Tracking

| # | Item | Status | PR |
|---|------|--------|-----|
| 1 | E2E integration test | Not started | |
| 2 | Non-public issue test | Not started | |
| 3 | Log remote link errors | Not started | |
| 4 | Domain filter test | Not started | |
| 5 | CI test failures | Not started | |
| 6 | ADF code dedup | Not started | |
| 7 | README accuracy | Not started | |
| 8 | Newtypes | Not started | |
| 9 | ADF edge case tests | Not started | |
| 10 | Mutex handling | Not started | |
