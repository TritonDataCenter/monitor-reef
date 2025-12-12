<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# JIRA Stub Server

A Dropshot-based HTTP server that implements the JIRA API trait with static test data. This enables integration testing of bugview-service without requiring a real JIRA instance.

## Use Cases

- **Integration testing**: Test bugview-service end-to-end without JIRA
- **Local development**: Run a complete bugview stack locally
- **Demos**: Show bugview functionality with realistic sample data
- **CLI testing**: Test bugview-cli against predictable data

## Quick Start

### Run the stub server

```bash
cargo run -p jira-stub-server
```

The server starts on `http://localhost:9090` and exposes:
- `GET /rest/api/3/search/jql?jql=...` - Search issues
- `GET /rest/api/3/issue/{issueIdOrKey}` - Get issue details
- `GET /rest/api/3/issue/{issueIdOrKey}/remotelink` - Get remote links

### Run bugview against the stub

**Terminal 1: Start the JIRA stub server**

```bash
cargo run -p jira-stub-server
```

You should see:
```
INFO Loaded 5 issues from fixtures
INFO Stub JIRA server listening on http://localhost:9090
INFO Available endpoints:
INFO   GET /rest/api/3/search/jql?jql=...
INFO   GET /rest/api/3/issue/{issueIdOrKey}
INFO   GET /rest/api/3/issue/{issueIdOrKey}/remotelink
```

**Terminal 2: Start bugview-service pointing at the stub**

```bash
JIRA_URL=http://localhost:9090 \
JIRA_USERNAME=unused \
JIRA_PASSWORD=unused \
JIRA_DEFAULT_LABEL=public \
JIRA_ALLOWED_LABELS=public,rust \
JIRA_ALLOWED_DOMAINS=github.com,smartos.org \
cargo run -p bugview-service
```

You should see:
```
INFO bugview_service: Initializing JIRA client
INFO bugview_service: Initializing HTML renderer
INFO bugview_service: Bugview service running on http://127.0.0.1:8080
```

**Terminal 3: Query bugview**

```bash
# List all public issues (JSON)
curl -s http://localhost:8080/bugview/index.json | jq .

# View a specific issue (HTML)
curl http://localhost:8080/bugview/issue/OS-8627

# View a specific issue (JSON summary)
curl -s http://localhost:8080/bugview/json/OS-8627 | jq .

# Open in browser
open http://localhost:8080/bugview/
```

**Using bugview-cli**

```bash
# List issues
cargo run -p bugview-cli -- --base-url http://localhost:8080 list

# Get a specific issue
cargo run -p bugview-cli -- --base-url http://localhost:8080 get OS-8627
```

## Fixture Data

Sample issues are loaded from `fixtures/issues.json`. The fixture data is inspired by real issues from the SmartOS bugview instance at `smartos.org/bugview`.

### Available Issues

| Key | Summary |
|-----|---------|
| OS-8627 | dlpi_open_zone() messes up DLS reference holds |
| TRITON-1813 | non-integer 'refreservation' values confound sdc-migrate |
| OS-8638 | Want ability to specify zfs_arc_max |
| TOOLS-2590 | Rewrite Bugview in Rust |
| MANTA-5480 | Objects are not garbage-collected when replaced |

### Adding Custom Fixtures

Edit `fixtures/issues.json` to add more test issues. The format follows the JIRA REST API v3 structure:

```json
{
  "PROJ-123": {
    "key": "PROJ-123",
    "id": "10001",
    "fields": {
      "summary": "Issue title",
      "status": { "name": "Open" },
      "labels": ["public"],
      "created": "2025-01-01T00:00:00.000-0500",
      "updated": "2025-01-02T00:00:00.000-0500",
      "description": { "type": "doc", "version": 1, "content": [...] }
    },
    "renderedFields": {
      "description": "<p>HTML rendered description</p>"
    }
  }
}
```

Remote links go in `fixtures/remote_links.json`:

```json
{
  "PROJ-123": [
    {
      "id": 1,
      "object": {
        "url": "https://github.com/example/repo/pull/1",
        "title": "Related PR"
      }
    }
  ]
}
```

## Library Usage

The stub server can also be used as a library in tests:

```rust
use jira_stub_server::{StubContext, api_description};
use std::sync::Arc;

#[tokio::test]
async fn test_with_stub_jira() {
    let fixtures_dir = Path::new("path/to/fixtures");
    let context = Arc::new(StubContext::from_fixtures(&fixtures_dir).unwrap());
    let api = api_description().unwrap();

    // Create dropshot server with the stub context...
}
```

## Limitations

- No authentication (all requests succeed)
- Basic JQL parsing (only `labels IN (...)` is supported)
- No pagination cursor support (always returns all matching results)
- Read-only (no create/update/delete operations)
