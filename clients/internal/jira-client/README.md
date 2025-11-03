# JIRA Client (Progenitor-Generated)

**IMPORTANT**: This client provides access to a *subset* of the JIRA REST API v3, not the complete API.

## Purpose

This is a Progenitor-generated type-safe client for the JIRA API subset defined in `apis/jira-api`. It provides access to only the endpoints used by bugview-service:
- Search issues using JQL queries
- Get full issue details
- Get remote links associated with issues

## Why This Exists

Instead of using a massive auto-generated client from JIRA's complete OpenAPI spec (which often fails to compile), we:
1. Define only the endpoints we need in `apis/jira-api`
2. Generate a clean OpenAPI spec via dropshot-api-manager
3. Use Progenitor to create a reliable, type-safe client
4. Get fast compile times and better maintainability

## Usage

This is a library crate meant to be consumed by services. The bugview-service wraps this client to add authentication and maintain a clean interface.

### As a Library Dependency

```rust
use jira_client::Client;

let client = Client::new_with_client(
    "https://jira.example.com",
    authenticated_reqwest_client
);

// Search for issues
let response = client
    .search_issues()
    .jql("project = FOO AND labels = public")
    .max_results(50)
    .send()
    .await?;
```

## Authentication

This client doesn't handle authentication directly. Pass a pre-configured reqwest client with authentication headers using `new_with_client()`.

## Relationship to Full JIRA API

The `clients/external/jira-client` directory (disabled) attempted to use the full JIRA API spec with Progenitor, but:
- The spec is >1MB and causes compilation issues
- We only need 3 endpoints
- Hand-written clients are more maintainable for small API surfaces

This approach (define subset → generate clean spec → generate client) proved far more practical.

## Generated Code

The client code is generated at build time from `openapi-specs/generated/jira-api.json`. To regenerate:

```bash
# From monorepo root
cargo run -p openapi-manager -- generate --blessed-from-dir openapi-manager/openapi-specs-blessed
cargo build -p jira-client
```

## Reference

- Source trait: `apis/jira-api/src/lib.rs`
- [Progenitor Documentation](https://github.com/oxidecomputer/progenitor)
- [JIRA REST API v3](https://developer.atlassian.com/cloud/jira/platform/rest/v3/)
